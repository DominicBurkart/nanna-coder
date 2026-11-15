//! Required default model and Gemma 4 auto-selection (message hub design 8.5).

use crate::provider::ModelError;
use async_trait::async_trait;

/// Hardware accelerator class of the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accel {
    Cpu,
    Cuda,
    Rocm,
    Metal,
}

/// Memory a device offers to a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeviceCapacity {
    pub ram_bytes: u64,
    pub vram_bytes: Option<u64>,
    pub accel: Accel,
}

impl DeviceCapacity {
    /// Bytes a model must fit in.
    pub fn usable_bytes(&self) -> u64 {
        match self.accel {
            Accel::Cpu => self.ram_bytes,
            Accel::Cuda | Accel::Rocm | Accel::Metal => self.vram_bytes.unwrap_or(self.ram_bytes),
        }
    }
}

/// One runnable model variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelVariant {
    pub tag: String,
    pub params: u64,
    pub min_memory_bytes: u64,
}

/// An explicitly configured model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelConfig {
    pub name: String,
}

/// The resolved default model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefaultModel {
    Configured(ModelConfig),
    AutoGemma4(ModelVariant),
}

impl DefaultModel {
    /// Name to hand to the runtime.
    pub fn name(&self) -> &str {
        match self {
            DefaultModel::Configured(c) => &c.name,
            DefaultModel::AutoGemma4(v) => &v.tag,
        }
    }
}

/// Source of Gemma 4 variants and their sizes.
#[async_trait]
pub trait VariantCatalog: Send + Sync {
    async fn gemma4_variants(&self) -> Result<Vec<ModelVariant>, ModelError>;
}

/// Parse a device from `/proc/meminfo`-style text and optional VRAM.
///
/// ```
/// use model::default_model::{parse_capacity, Accel};
/// let cap = parse_capacity("MemTotal: 16777216 kB\n", Some((Accel::Cuda, 8 << 30))).unwrap();
/// assert_eq!(cap.ram_bytes, 16 << 30);
/// assert_eq!(cap.vram_bytes, Some(8 << 30));
/// ```
pub fn parse_capacity(
    meminfo: &str,
    gpu: Option<(Accel, u64)>,
) -> Result<DeviceCapacity, ModelError> {
    let bad = |why: &str| ModelError::Unknown {
        message: format!("cannot read device memory: {why}"),
    };
    let line = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemTotal:"))
        .ok_or_else(|| bad("no MemTotal line"))?;
    let mut fields = line.split_whitespace();
    let amount: u64 = fields
        .next()
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| bad("MemTotal is not a number"))?;
    if !fields.next().is_some_and(|u| u.eq_ignore_ascii_case("kB")) || fields.next().is_some() {
        return Err(bad("MemTotal is not in kB"));
    }
    let ram_bytes = amount
        .checked_mul(1024)
        .ok_or_else(|| bad("MemTotal overflows"))?;
    if ram_bytes == 0 {
        return Err(bad("MemTotal is zero"));
    }
    let (accel, vram_bytes) = match gpu {
        Some((accel, vram)) if accel != Accel::Cpu => (accel, Some(vram)),
        _ => (Accel::Cpu, None),
    };
    Ok(DeviceCapacity {
        ram_bytes,
        vram_bytes,
        accel,
    })
}

/// Largest VRAM figure in `nvidia-smi --query-gpu=memory.total --format=csv,noheader,nounits` output (MiB per line).
///
/// ```
/// assert_eq!(model::default_model::parse_nvidia_smi("8192\n24576\n"), Some(24576 << 20));
/// assert_eq!(model::default_model::parse_nvidia_smi("garbage"), None);
/// ```
pub fn parse_nvidia_smi(output: &str) -> Option<u64> {
    output
        .lines()
        .filter_map(|l| l.trim().parse::<u64>().ok())
        .filter(|mib| *mib > 0)
        .max()
        .and_then(|mib| mib.checked_mul(1 << 20))
}

fn read_gpu() -> Option<(Accel, u64)> {
    let smi = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| parse_nvidia_smi(&String::from_utf8_lossy(&o.stdout)));
    if let Some(vram) = smi {
        return Some((Accel::Cuda, vram));
    }
    let mut best: Option<u64> = None;
    for entry in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let dir = entry.path().join("device");
        let vendor = std::fs::read_to_string(dir.join("vendor")).unwrap_or_default();
        if vendor.trim() != "0x1002" {
            continue;
        }
        let total = std::fs::read_to_string(dir.join("mem_info_vram_total"))
            .ok()
            .and_then(|t| t.trim().parse::<u64>().ok())
            .filter(|b| *b > 0);
        best = best.max(total);
    }
    best.map(|vram| (Accel::Rocm, vram))
}

/// Probe this machine: Linux `/proc/meminfo`, `nvidia-smi` and AMD sysfs VRAM. Errors on platforms without `/proc/meminfo`.
pub fn probe_device() -> Result<DeviceCapacity, ModelError> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").map_err(|e| ModelError::Unknown {
        message: format!("cannot read /proc/meminfo: {e}"),
    })?;
    parse_capacity(&meminfo, read_gpu())
}

/// Largest-parameter variant whose memory need plus 20% headroom fits the device (VRAM when an accelerator is present, else RAM). Ties on parameters pick the smaller footprint.
///
/// ```
/// use model::default_model::*;
/// let cap = DeviceCapacity { ram_bytes: 16 << 30, vram_bytes: None, accel: Accel::Cpu };
/// let variants = vec![
///     ModelVariant { tag: "gemma4:small".into(), params: 4, min_memory_bytes: 4 << 30 },
///     ModelVariant { tag: "gemma4:big".into(), params: 30, min_memory_bytes: 20 << 30 },
/// ];
/// assert_eq!(select_default(&cap, &variants).unwrap().tag, "gemma4:small");
/// ```
pub fn select_default(cap: &DeviceCapacity, variants: &[ModelVariant]) -> Option<ModelVariant> {
    let usable = u128::from(cap.usable_bytes()) * 5;
    variants
        .iter()
        .filter(|v| u128::from(v.min_memory_bytes) * 6 <= usable)
        .max_by(|a, b| {
            a.params
                .cmp(&b.params)
                .then(b.min_memory_bytes.cmp(&a.min_memory_bytes))
                .then(b.tag.cmp(&a.tag))
        })
        .cloned()
}

/// Resolve the default model: the configured one, else the largest runnable Gemma 4. Refuses when neither exists.
///
/// ```
/// # tokio_test::block_on(async {
/// use model::default_model::*;
/// let cap = DeviceCapacity { ram_bytes: 1 << 30, vram_bytes: None, accel: Accel::Cpu };
/// let cat = FixtureCatalog::default();
/// let cfg = Some(ModelConfig { name: "qwen3:0.6b".into() });
/// assert_eq!(resolve_default_model(cfg, &cap, &cat).await.unwrap().name(), "qwen3:0.6b");
/// assert!(resolve_default_model(None, &cap, &cat).await.is_err());
/// # });
/// ```
pub async fn resolve_default_model(
    cfg: Option<ModelConfig>,
    cap: &DeviceCapacity,
    cat: &dyn VariantCatalog,
) -> Result<DefaultModel, ModelError> {
    if let Some(cfg) = cfg.filter(|c| !c.name.trim().is_empty()) {
        return Ok(DefaultModel::Configured(cfg));
    }
    let refuse = |why: String| ModelError::InvalidConfig {
        message: format!(
            "no default model is configured and {why}; configure one with --model or NANNA_MODEL"
        ),
    };
    let variants = cat
        .gemma4_variants()
        .await
        .map_err(|e| refuse(format!("the Gemma 4 catalog is unavailable ({e})")))?;
    select_default(cap, &variants)
        .map(DefaultModel::AutoGemma4)
        .ok_or_else(|| refuse("no Gemma 4 variant fits this device".to_string()))
}

/// Build a variant from registry manifest and config blob JSON. Memory need is the model, projector and draft layers.
pub fn variant_from_registry(
    tag: &str,
    manifest_json: &str,
    config_json: &str,
) -> Result<ModelVariant, ModelError> {
    let bad = |why: &str| ModelError::Unknown {
        message: format!("registry metadata for {tag}: {why}"),
    };
    let manifest: serde_json::Value = serde_json::from_str(manifest_json)?;
    let config: serde_json::Value = serde_json::from_str(config_json)?;
    let layers = manifest
        .get("layers")
        .and_then(|l| l.as_array())
        .ok_or_else(|| bad("no layers"))?;
    let mut has_model = false;
    let mut total: u64 = 0;
    for layer in layers {
        let kind = layer
            .get("mediaType")
            .and_then(|m| m.as_str())
            .unwrap_or("");
        let size = layer.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
        match kind {
            "application/vnd.ollama.image.model" => {
                has_model = true;
                total = total.saturating_add(size);
            }
            "application/vnd.ollama.image.projector" | "application/vnd.ollama.image.draft" => {
                total = total.saturating_add(size);
            }
            _ => {}
        }
    }
    if !has_model || total == 0 {
        return Err(bad("no model layer"));
    }
    let params = config
        .get("model_type")
        .and_then(|t| t.as_str())
        .and_then(parse_params)
        .ok_or_else(|| bad("no parsable model_type"))?;
    Ok(ModelVariant {
        tag: tag.to_string(),
        params,
        min_memory_bytes: total,
    })
}

/// Parse a parameter count such as `7.5B`, `270M`.
///
/// ```
/// assert_eq!(model::default_model::parse_params("7.5B"), Some(7_500_000_000));
/// assert_eq!(model::default_model::parse_params("7.5"), None);
/// ```
pub fn parse_params(text: &str) -> Option<u64> {
    let text = text.trim();
    let unit = text.chars().last()?;
    let scale = match unit.to_ascii_uppercase() {
        'K' => 1e3,
        'M' => 1e6,
        'B' => 1e9,
        'T' => 1e12,
        _ => return None,
    };
    let number: f64 = text[..text.len() - unit.len_utf8()].trim().parse().ok()?;
    let value = number * scale;
    (number.is_finite() && number > 0.0 && value.is_finite() && value < u64::MAX as f64)
        .then(|| value.round() as u64)
}

/// Fixture catalog for tests.
#[derive(Debug, Clone, Default)]
pub struct FixtureCatalog {
    pub variants: Vec<ModelVariant>,
    pub fail: bool,
}

#[async_trait]
impl VariantCatalog for FixtureCatalog {
    async fn gemma4_variants(&self) -> Result<Vec<ModelVariant>, ModelError> {
        if self.fail {
            return Err(ModelError::ServiceUnavailable {
                message: "fixture".into(),
            });
        }
        Ok(self.variants.clone())
    }
}

/// Tag names tried against the registry.
pub const GEMMA4_TAGS: &[&str] = &["e2b", "e4b", "12b", "26b", "31b"];

/// Public Ollama registry.
pub const DEFAULT_REGISTRY_URL: &str = "https://registry.ollama.ai";

/// Live catalog over the Ollama registry.
#[derive(Debug, Clone)]
pub struct OllamaRegistryCatalog {
    registry_url: String,
}

impl OllamaRegistryCatalog {
    pub fn new(registry_url: impl Into<String>) -> Self {
        Self {
            registry_url: registry_url.into(),
        }
    }
}

#[async_trait]
impl VariantCatalog for OllamaRegistryCatalog {
    async fn gemma4_variants(&self) -> Result<Vec<ModelVariant>, ModelError> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .build()?;
        let base = self.registry_url.trim_end_matches('/');
        let mut variants = Vec::new();
        let mut last_error = None;
        for tag in GEMMA4_TAGS {
            match fetch_variant(&client, base, tag).await {
                Ok(variant) => variants.push(variant),
                Err(e) => {
                    tracing::warn!("skipping gemma4:{tag}: {e}");
                    last_error = Some(e);
                }
            }
        }
        match (variants.is_empty(), last_error) {
            (true, Some(e)) => Err(e),
            (true, None) => Err(ModelError::ServiceUnavailable {
                message: "no Gemma 4 tags to query".into(),
            }),
            _ => Ok(variants),
        }
    }
}

async fn fetch_variant(
    client: &reqwest::Client,
    base: &str,
    tag: &str,
) -> Result<ModelVariant, ModelError> {
    let manifest = client
        .get(format!("{base}/v2/library/gemma4/manifests/{tag}"))
        .header(
            "Accept",
            "application/vnd.docker.distribution.manifest.v2+json",
        )
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let digest = serde_json::from_str::<serde_json::Value>(&manifest)?
        .pointer("/config/digest")
        .and_then(|d| d.as_str())
        .map(str::to_string)
        .filter(|d| {
            d.strip_prefix("sha256:")
                .is_some_and(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()))
        })
        .ok_or_else(|| ModelError::Unknown {
            message: format!("registry manifest for gemma4:{tag} has no usable config digest"),
        })?;
    let config = client
        .get(format!("{base}/v2/library/gemma4/blobs/{digest}"))
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    variant_from_registry(&format!("gemma4:{tag}"), &manifest, &config)
}
