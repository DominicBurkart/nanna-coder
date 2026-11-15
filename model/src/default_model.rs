//! Required default model and Gemma 4 auto-selection (message hub design 8.5).

use crate::provider::ModelError;
use crate::provider::ModelProvider;
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

/// Longest `nvidia-smi` is given before the probe refuses.
pub const NVIDIA_SMI_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

fn query_nvidia_smi(
    program: &str,
    timeout: std::time::Duration,
) -> Result<Option<u64>, ModelError> {
    use std::io::Read;
    let mut child = match std::process::Command::new(program)
        .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return Ok(None)
        }
        Err(e) => {
            return Err(ModelError::Unknown {
                message: format!("cannot run {program}: {e}"),
            })
        }
    };
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(out) = stdout.as_mut() {
            let _ = out.by_ref().take(64 * 1024).read_to_string(&mut text);
        }
        text
    });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(ModelError::Unknown {
                    message: format!(
                        "{program} did not answer within {}s and was killed",
                        timeout.as_secs_f32()
                    ),
                });
            }
            Err(e) => {
                return Err(ModelError::Unknown {
                    message: format!("cannot wait for {program}: {e}"),
                })
            }
        }
    };
    let text = reader.join().unwrap_or_default();
    Ok(status.success().then(|| parse_nvidia_smi(&text)).flatten())
}

fn read_amd_vram() -> Option<u64> {
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
    best
}

/// Probe this machine: Linux `/proc/meminfo`, `nvidia-smi` (bounded by [`NVIDIA_SMI_TIMEOUT`]) and AMD sysfs VRAM. Errors on platforms without `/proc/meminfo`, naming the platform, and when `nvidia-smi` hangs. Blocking: call from `spawn_blocking` in async code.
pub fn probe_device() -> Result<DeviceCapacity, ModelError> {
    probe_device_with("nvidia-smi", NVIDIA_SMI_TIMEOUT)
}

/// [`probe_device`] with an explicit `nvidia-smi` program and time limit. A missing or failing program means no NVIDIA GPU; a program that outlives `timeout` is killed and is an error.
pub fn probe_device_with(
    nvidia_smi: &str,
    timeout: std::time::Duration,
) -> Result<DeviceCapacity, ModelError> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").map_err(|e| ModelError::Unknown {
        message: format!(
            "device probing needs /proc/meminfo (supported on Linux; this platform is {}): {e}",
            std::env::consts::OS
        ),
    })?;
    let gpu = match query_nvidia_smi(nvidia_smi, timeout)? {
        Some(vram) => Some((Accel::Cuda, vram)),
        None => read_amd_vram().map(|vram| (Accel::Rocm, vram)),
    };
    parse_capacity(&meminfo, gpu)
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

/// Largest registry manifest accepted.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;
/// Largest registry config blob accepted.
pub const MAX_CONFIG_BYTES: usize = 64 * 1024;

fn is_loopback(url: &reqwest::Url) -> bool {
    url.host_str().is_some_and(|host| {
        host == "localhost"
            || host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

fn registry_base(url: &str) -> Result<String, ModelError> {
    let bad = |why: &str| ModelError::InvalidConfig {
        message: format!("registry URL rejected: {why}"),
    };
    let parsed = reqwest::Url::parse(url).map_err(|_| bad("not a URL"))?;
    let loopback = is_loopback(&parsed);
    match parsed.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return Err(bad("only https is allowed (plain http only for loopback)")),
    }
    Ok(url.trim_end_matches('/').to_string())
}

async fn read_capped(
    mut response: reqwest::Response,
    cap: usize,
    what: &str,
) -> Result<String, ModelError> {
    let status = response.status();
    if !status.is_success() {
        return Err(ModelError::ServiceUnavailable {
            message: format!("registry answered {what} with status {status}"),
        });
    }
    let too_big = || ModelError::Unknown {
        message: format!("registry {what} exceeds the {cap} byte limit"),
    };
    if response.content_length().is_some_and(|l| l > cap as u64) {
        return Err(too_big());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > cap {
            return Err(too_big());
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|_| ModelError::Unknown {
        message: format!("registry {what} is not UTF-8"),
    })
}

#[async_trait]
impl VariantCatalog for OllamaRegistryCatalog {
    async fn gemma4_variants(&self) -> Result<Vec<ModelVariant>, ModelError> {
        let base = registry_base(&self.registry_url)?;
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        let mut variants = Vec::new();
        let mut last_error = None;
        for tag in GEMMA4_TAGS {
            match fetch_variant(&client, &base, tag).await {
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
    let response = client
        .get(format!("{base}/v2/library/gemma4/manifests/{tag}"))
        .header(
            "Accept",
            "application/vnd.docker.distribution.manifest.v2+json",
        )
        .send()
        .await?;
    let manifest = read_capped(response, MAX_MANIFEST_BYTES, "manifest").await?;
    let parsed = serde_json::from_str::<serde_json::Value>(&manifest)?;
    let digest = parsed
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
    let declared = parsed.pointer("/config/size").and_then(|s| s.as_u64());
    let response = get_blob(client, &format!("{base}/v2/library/gemma4/blobs/{digest}")).await?;
    let config = read_capped(response, MAX_CONFIG_BYTES, "config blob").await?;
    if declared != Some(config.len() as u64) {
        return Err(ModelError::Unknown {
            message: format!(
                "registry config blob for gemma4:{tag} is {} bytes but the manifest declares {declared:?}",
                config.len()
            ),
        });
    }
    variant_from_registry(&format!("gemma4:{tag}"), &manifest, &config)
}

/// Verify that an auto-selected model is present in the runtime (`/api/tags`). A configured model is not checked, since it may be hosted. Refuses with an actionable `ollama pull` message rather than letting every audit fail closed later.
pub async fn ensure_runnable(
    provider: &dyn ModelProvider,
    model: &DefaultModel,
) -> Result<(), ModelError> {
    let DefaultModel::AutoGemma4(variant) = model else {
        return Ok(());
    };
    let present = provider
        .list_models()
        .await
        .map_err(|e| ModelError::InvalidConfig {
            message: format!(
                "cannot confirm that {} is available because the model runtime (ollama) is unreachable ({e}); start ollama and run `ollama pull {}`, or configure a model with --model or NANNA_MODEL",
                variant.tag, variant.tag
            ),
        })?;
    if present.iter().any(|m| m.name == variant.tag) {
        Ok(())
    } else {
        Err(ModelError::InvalidConfig {
            message: format!(
                "the auto-selected default model {} is not available in the runtime; run `ollama pull {}`, or configure a model with --model or NANNA_MODEL",
                variant.tag, variant.tag
            ),
        })
    }
}

async fn get_blob(client: &reqwest::Client, url: &str) -> Result<reqwest::Response, ModelError> {
    let first = client.get(url).send().await?;
    if !first.status().is_redirection() {
        return Ok(first);
    }
    let refuse = |why: &str| ModelError::Unknown {
        message: format!("registry redirect refused: {why}"),
    };
    let origin = reqwest::Url::parse(url).map_err(|_| refuse("unparsable registry URL"))?;
    let target = first
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|location| origin.join(location).ok())
        .ok_or_else(|| refuse("no usable Location header"))?;
    if let Some(reason) = redirect_refusal(&origin, &target) {
        return Err(refuse(reason));
    }
    Ok(client.get(target).send().await?)
}

fn redirect_refusal(origin: &reqwest::Url, target: &reqwest::Url) -> Option<&'static str> {
    let loopback_only = target.scheme() == "http" && is_loopback(target) && is_loopback(origin);
    if loopback_only {
        return None;
    }
    if target.scheme() != "https" {
        return Some("the target is not https");
    }
    if !target.username().is_empty() || target.password().is_some() {
        return Some("the target carries userinfo");
    }
    if target.port().is_some() {
        return Some("the target uses a non-default port");
    }
    let Some(host) = target.host_str() else {
        return Some("the target host is not a DNS name");
    };
    let host = host.trim_end_matches('.');
    let ip_literal = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::IpAddr>()
        .is_ok();
    if ip_literal {
        return Some("the target host is an IP address");
    }
    let local = host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal");
    if local {
        return Some("the target host is a local name");
    }
    if !host.contains('.') {
        return Some("the target host is not a DNS name");
    }
    None
}

#[cfg(test)]
mod redirect_tests {
    use super::redirect_refusal;

    fn refusal(origin: &str, target: &str) -> Option<&'static str> {
        redirect_refusal(
            &reqwest::Url::parse(origin).unwrap(),
            &reqwest::Url::parse(target).unwrap(),
        )
    }

    const REGISTRY: &str = "https://registry.ollama.ai/v2/library/gemma4/blobs/sha256:ab";
    const LOOPBACK: &str = "http://127.0.0.1:8080/v2/x";

    macro_rules! refused {
        ($name:ident, $target:expr, $reason:expr) => {
            #[test]
            fn $name() {
                assert_eq!(refusal(REGISTRY, $target), Some($reason));
                assert_eq!(refusal(LOOPBACK, $target), Some($reason));
            }
        };
    }

    refused!(
        an_ipv4_literal_is_refused,
        "https://127.0.0.1/d",
        "the target host is an IP address"
    );
    refused!(
        a_public_ipv4_literal_is_refused,
        "https://93.184.216.34/d",
        "the target host is an IP address"
    );
    refused!(
        an_ipv6_literal_is_refused,
        "https://[::1]/d",
        "the target host is an IP address"
    );
    refused!(
        a_global_ipv6_literal_is_refused,
        "https://[2001:db8::1]/d",
        "the target host is an IP address"
    );
    refused!(
        a_decimal_numeric_host_is_refused,
        "https://2130706433/d",
        "the target host is an IP address"
    );
    refused!(
        a_hex_numeric_host_is_refused,
        "https://0x7f.0.0.1/d",
        "the target host is an IP address"
    );
    refused!(
        an_octal_numeric_host_is_refused,
        "https://0177.0.0.1/d",
        "the target host is an IP address"
    );
    refused!(
        localhost_is_refused,
        "https://localhost/d",
        "the target host is a local name"
    );
    refused!(
        a_trailing_dot_localhost_is_refused,
        "https://localhost./d",
        "the target host is a local name"
    );
    refused!(
        a_localhost_subdomain_is_refused,
        "https://a.localhost/d",
        "the target host is a local name"
    );
    refused!(
        a_dot_local_host_is_refused,
        "https://printer.local/d",
        "the target host is a local name"
    );
    refused!(
        a_dot_internal_host_is_refused,
        "https://svc.internal/d",
        "the target host is a local name"
    );
    refused!(
        a_mixed_case_local_host_is_refused,
        "https://Printer.LOCAL/d",
        "the target host is a local name"
    );
    refused!(
        a_single_label_host_is_refused,
        "https://intranet/d",
        "the target host is not a DNS name"
    );
    refused!(
        userinfo_with_a_password_is_refused,
        "https://user:pw@storage.example.com/d",
        "the target carries userinfo"
    );
    refused!(
        userinfo_without_a_password_is_refused,
        "https://user@storage.example.com/d",
        "the target carries userinfo"
    );
    refused!(
        a_non_default_port_is_refused,
        "https://storage.example.com:8443/d",
        "the target uses a non-default port"
    );
    refused!(
        a_password_only_userinfo_is_refused,
        "https://:pw@storage.example.com/d",
        "the target carries userinfo"
    );
    refused!(
        plain_http_is_refused,
        "http://storage.example.com/d",
        "the target is not https"
    );

    #[test]
    fn a_dns_hostname_over_https_with_a_signed_query_is_accepted() {
        let target = "https://dd20bb89.r2.cloudflarestorage.com/ollama/data?X-Amz-Signature=ab";
        assert_eq!(refusal(REGISTRY, target), None);
        assert_eq!(refusal(LOOPBACK, target), None);
    }

    #[test]
    fn an_explicit_default_https_port_is_accepted() {
        assert_eq!(refusal(REGISTRY, "https://storage.example.com:443/d"), None);
    }

    #[test]
    fn the_plain_http_exception_needs_a_loopback_origin_and_a_loopback_target() {
        assert_eq!(refusal(LOOPBACK, "http://127.0.0.1:9/d"), None);
        assert_eq!(refusal(LOOPBACK, "http://localhost:9/d"), None);
        assert_eq!(
            refusal(REGISTRY, "http://127.0.0.1:9/d"),
            Some("the target is not https")
        );
        assert_eq!(
            refusal(LOOPBACK, "http://storage.example.com/d"),
            Some("the target is not https")
        );
    }
}
