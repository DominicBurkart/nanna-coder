use model::default_model::*;
use model::ModelError;

const GIB: u64 = 1 << 30;
const B: u64 = 1_000_000_000;

fn v(tag: &str, params_b: u64, min_gib: u64) -> ModelVariant {
    ModelVariant {
        tag: tag.to_string(),
        params: params_b * B,
        min_memory_bytes: min_gib * GIB,
    }
}

fn gemma_set() -> Vec<ModelVariant> {
    vec![
        v("gemma4:e2b", 5, 4),
        v("gemma4:e4b", 8, 6),
        v("gemma4:26b", 25, 18),
        v("gemma4:31b", 31, 20),
    ]
}

fn cpu(ram_gib: u64) -> DeviceCapacity {
    DeviceCapacity {
        ram_bytes: ram_gib * GIB,
        vram_bytes: None,
        accel: Accel::Cpu,
    }
}

fn gpu(ram_gib: u64, vram_gib: u64) -> DeviceCapacity {
    DeviceCapacity {
        ram_bytes: ram_gib * GIB,
        vram_bytes: Some(vram_gib * GIB),
        accel: Accel::Cuda,
    }
}

fn picked(cap: &DeviceCapacity) -> Option<String> {
    select_default(cap, &gemma_set()).map(|v| v.tag)
}

#[test]
fn select_default_table() {
    assert_eq!(picked(&cpu(4)), None);
    assert_eq!(picked(&cpu(5)).as_deref(), Some("gemma4:e2b"));
    assert_eq!(picked(&cpu(6)).as_deref(), Some("gemma4:e2b"));
    assert_eq!(picked(&cpu(8)).as_deref(), Some("gemma4:e4b"));
    assert_eq!(picked(&cpu(21)).as_deref(), Some("gemma4:e4b"));
    assert_eq!(picked(&cpu(22)).as_deref(), Some("gemma4:26b"));
    assert_eq!(picked(&cpu(24)).as_deref(), Some("gemma4:31b"));
    assert_eq!(picked(&cpu(128)).as_deref(), Some("gemma4:31b"));
}

#[test]
fn headroom_boundary_is_exactly_twenty_percent() {
    let variants = vec![ModelVariant {
        tag: "gemma4:x".into(),
        params: 1,
        min_memory_bytes: 1000,
    }];
    let at = |ram| DeviceCapacity {
        ram_bytes: ram,
        vram_bytes: None,
        accel: Accel::Cpu,
    };
    assert!(select_default(&at(1200), &variants).is_some());
    assert!(select_default(&at(1199), &variants).is_none());
}

#[test]
fn an_accelerator_is_judged_by_vram_not_ram() {
    assert_eq!(
        select_default(&gpu(128, 8), &gemma_set()).map(|v| v.tag),
        Some("gemma4:e4b".to_string())
    );
    assert_eq!(select_default(&gpu(128, 4), &gemma_set()), None);
}

#[test]
fn a_cpu_device_ignores_stray_vram() {
    let cap = DeviceCapacity {
        ram_bytes: 6 * GIB,
        vram_bytes: Some(128 * GIB),
        accel: Accel::Cpu,
    };
    assert_eq!(
        select_default(&cap, &gemma_set()).map(|v| v.tag),
        Some("gemma4:e2b".to_string())
    );
}

#[test]
fn unified_memory_accelerator_without_vram_uses_ram() {
    let cap = DeviceCapacity {
        ram_bytes: 32 * GIB,
        vram_bytes: None,
        accel: Accel::Metal,
    };
    assert_eq!(
        select_default(&cap, &gemma_set()).map(|v| v.tag),
        Some("gemma4:31b".to_string())
    );
}

#[test]
fn empty_catalog_selects_nothing() {
    assert_eq!(select_default(&cpu(1024), &[]), None);
}

#[test]
fn extreme_sizes_do_not_overflow() {
    let variants = vec![ModelVariant {
        tag: "gemma4:huge".into(),
        params: u64::MAX,
        min_memory_bytes: u64::MAX,
    }];
    let cap = DeviceCapacity {
        ram_bytes: u64::MAX,
        vram_bytes: None,
        accel: Accel::Cpu,
    };
    assert_eq!(select_default(&cap, &variants), None);
}

#[test]
fn ties_on_params_pick_the_smaller_footprint() {
    let variants = vec![v("gemma4:fat", 8, 10), v("gemma4:lean", 8, 6)];
    assert_eq!(
        select_default(&cpu(64), &variants).map(|v| v.tag),
        Some("gemma4:lean".to_string())
    );
}

const MEMINFO: &str =
    "MemTotal:       32768000 kB\nMemFree:         1000 kB\nMemAvailable:   20000000 kB\n";

#[test]
fn meminfo_parses_memtotal_in_bytes() {
    let cap = parse_capacity(MEMINFO, None).unwrap();
    assert_eq!(cap.ram_bytes, 32_768_000 * 1024);
    assert_eq!(cap.vram_bytes, None);
    assert_eq!(cap.accel, Accel::Cpu);
}

#[test]
fn gpu_numbers_set_accelerator_and_vram() {
    let cap = parse_capacity(MEMINFO, Some((Accel::Rocm, 16 * GIB))).unwrap();
    assert_eq!(cap.accel, Accel::Rocm);
    assert_eq!(cap.vram_bytes, Some(16 * GIB));
}

#[test]
fn a_cpu_gpu_pair_drops_the_vram() {
    let cap = parse_capacity(MEMINFO, Some((Accel::Cpu, 16 * GIB))).unwrap();
    assert_eq!(cap.vram_bytes, None);
}

#[test]
fn malformed_meminfo_is_an_error() {
    for bad in [
        "",
        "MemFree: 1 kB\n",
        "MemTotal: lots kB\n",
        "MemTotal: 12 MB\n",
        "MemTotal: 0 kB\n",
        "MemTotal: -5 kB\n",
        "MemTotal: 99999999999999999999999 kB\n",
    ] {
        assert!(parse_capacity(bad, None).is_err(), "accepted {bad:?}");
    }
}

#[test]
fn param_counts_parse() {
    assert_eq!(parse_params("7.5B"), Some(7_500_000_000));
    assert_eq!(parse_params("31b"), Some(31_000_000_000));
    assert_eq!(parse_params("25.2B"), Some(25_200_000_000));
    assert_eq!(parse_params("270M"), Some(270_000_000));
    assert_eq!(parse_params("500K"), Some(500_000));
    assert_eq!(parse_params("1.5T"), Some(1_500_000_000_000));
    assert_eq!(parse_params(" 8B "), Some(8_000_000_000));
    for bad in ["", "B", "abc", "-3B", "NaNB", "8", "8X", "infB", "1e400B"] {
        assert_eq!(parse_params(bad), None, "accepted {bad:?}");
    }
}

const MANIFEST: &str = r#"{"schemaVersion":2,"config":{"digest":"sha256:aa","size":88},"layers":[
{"mediaType":"application/vnd.ollama.image.model","digest":"sha256:01","size":5493439296},
{"mediaType":"application/vnd.ollama.image.projector","digest":"sha256:02","size":991552256},
{"mediaType":"application/vnd.ollama.image.draft","digest":"sha256:03","size":98653280},
{"mediaType":"application/vnd.ollama.image.template","digest":"sha256:04","size":13},
{"mediaType":"application/vnd.ollama.image.license","digest":"sha256:05","size":11355}]}"#;
const CONFIG: &str =
    r#"{"model_format":"gguf","model_family":"gemma4","model_type":"7.5B","file_type":"Q4_K_M"}"#;

#[test]
fn registry_metadata_becomes_a_variant() {
    let variant = variant_from_registry("gemma4:e4b", MANIFEST, CONFIG).unwrap();
    assert_eq!(variant.tag, "gemma4:e4b");
    assert_eq!(variant.params, 7_500_000_000);
    assert_eq!(variant.min_memory_bytes, 5493439296 + 991552256 + 98653280);
}

#[test]
fn registry_metadata_without_a_model_layer_or_params_is_rejected() {
    let no_model = r#"{"layers":[{"mediaType":"application/vnd.ollama.image.license","size":5}]}"#;
    assert!(variant_from_registry("gemma4:e4b", no_model, CONFIG).is_err());
    let no_type = r#"{"model_family":"gemma4"}"#;
    assert!(variant_from_registry("gemma4:e4b", MANIFEST, no_type).is_err());
    assert!(variant_from_registry("gemma4:e4b", "not json", CONFIG).is_err());
    assert!(variant_from_registry("gemma4:e4b", MANIFEST, "{").is_err());
}

#[test]
fn registry_metadata_with_overflowing_sizes_is_rejected_or_saturates() {
    let huge = r#"{"layers":[
{"mediaType":"application/vnd.ollama.image.model","size":18446744073709551615},
{"mediaType":"application/vnd.ollama.image.projector","size":5}]}"#;
    let variant = variant_from_registry("gemma4:x", huge, CONFIG).unwrap();
    assert_eq!(variant.min_memory_bytes, u64::MAX);
}

fn configured(name: &str) -> Option<ModelConfig> {
    Some(ModelConfig { name: name.into() })
}

fn fixture(fail: bool) -> FixtureCatalog {
    FixtureCatalog {
        variants: gemma_set(),
        fail,
    }
}

#[tokio::test]
async fn a_configured_model_wins_and_the_catalog_is_not_consulted() {
    let resolved = resolve_default_model(configured("qwen3:0.6b"), &cpu(1), &fixture(true))
        .await
        .unwrap();
    assert_eq!(resolved.name(), "qwen3:0.6b");
    assert!(matches!(resolved, DefaultModel::Configured(_)));
}

#[tokio::test]
async fn without_a_configured_model_the_largest_runnable_gemma4_is_chosen() {
    let resolved = resolve_default_model(None, &cpu(24), &fixture(false))
        .await
        .unwrap();
    assert_eq!(resolved.name(), "gemma4:31b");
    assert!(matches!(resolved, DefaultModel::AutoGemma4(_)));
}

#[tokio::test]
async fn a_blank_configured_name_counts_as_unconfigured() {
    for blank in ["", "   ", "\n"] {
        let resolved = resolve_default_model(configured(blank), &cpu(8), &fixture(false))
            .await
            .unwrap();
        assert_eq!(resolved.name(), "gemma4:e4b");
    }
}

#[tokio::test]
async fn startup_refuses_when_nothing_is_configured_and_nothing_fits() {
    let err = resolve_default_model(None, &cpu(2), &fixture(false))
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(matches!(err, ModelError::InvalidConfig { .. }), "{text}");
    assert!(text.contains("no default model"), "{text}");
    assert!(text.contains("--model"), "{text}");
}

#[tokio::test]
async fn startup_refuses_when_the_catalog_is_unavailable_and_nothing_is_configured() {
    let err = resolve_default_model(None, &cpu(128), &fixture(true))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("no default model"), "{err}");
}

#[tokio::test]
async fn startup_refuses_with_an_empty_catalog() {
    let empty = FixtureCatalog::default();
    assert!(resolve_default_model(None, &cpu(128), &empty)
        .await
        .is_err());
}

fn serve_registry() -> String {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
            let (status, body) = if path.contains("/blobs/") {
                ("200 OK", CONFIG.to_string())
            } else if path.contains("/manifests/31b") {
                ("404 Not Found", "{}".to_string())
            } else {
                ("200 OK", MANIFEST.to_string())
            };
            let reply = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
        }
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn the_live_catalog_reads_sizes_from_registry_metadata_and_skips_missing_tags() {
    let catalog = OllamaRegistryCatalog::new(serve_registry());
    let variants = catalog.gemma4_variants().await.unwrap();
    let tags: Vec<_> = variants.iter().map(|v| v.tag.as_str()).collect();
    assert_eq!(
        tags,
        vec!["gemma4:e2b", "gemma4:e4b", "gemma4:12b", "gemma4:26b"]
    );
    assert!(variants.iter().all(|v| v.params == 7_500_000_000));
}

#[tokio::test]
async fn the_live_catalog_fails_when_the_registry_is_unreachable() {
    let catalog = OllamaRegistryCatalog::new("http://127.0.0.1:1");
    assert!(catalog.gemma4_variants().await.is_err());
}
