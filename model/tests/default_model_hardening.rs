use model::default_model::*;
use model::{ModelError, OllamaConfig, OllamaProvider};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

const MODEL_LAYER: &str = "application/vnd.ollama.image.model";
const CONFIG: &str = r#"{"model_type":"7.5B"}"#;

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    unannounced_length: bool,
    split_at: Option<usize>,
}

fn ok(body: impl Into<Vec<u8>>) -> Reply {
    Reply {
        status: 200,
        headers: Vec::new(),
        body: body.into(),
        unannounced_length: false,
        split_at: None,
    }
}

type Seen = Arc<Mutex<Vec<String>>>;

fn spawn_stub(handler: impl Fn(&str) -> Reply + Send + 'static) -> (String, Seen) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let head = String::from_utf8_lossy(&buf[..n]).to_string();
            let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
            log.lock().unwrap().push(path.clone());
            let reply = handler(&path);
            let mut out = format!("HTTP/1.1 {} X\r\nconnection: close\r\n", reply.status);
            if !reply.unannounced_length {
                out.push_str(&format!("content-length: {}\r\n", reply.body.len()));
            }
            for (k, v) in &reply.headers {
                out.push_str(&format!("{k}: {v}\r\n"));
            }
            out.push_str("\r\n");
            let _ = stream.write_all(out.as_bytes());
            let at = split_point(&reply);
            let _ = stream.write_all(&reply.body[..at]);
            let _ = stream.flush();
            if at < reply.body.len() {
                std::thread::sleep(Duration::from_millis(100));
                let _ = stream.write_all(&reply.body[at..]);
            }
        }
    });
    (format!("http://{addr}"), seen)
}

fn split_point(reply: &Reply) -> usize {
    let len = reply.body.len();
    reply.split_at.map_or(len, |at| at.min(len))
}

fn manifest(config_size: usize, pad: usize) -> String {
    format!(
        r#"{{"config":{{"digest":"sha256:{}","size":{config_size}}},"layers":[{{"mediaType":"{MODEL_LAYER}","size":1000}}],"pad":"{}"}}"#,
        "ab".repeat(32),
        "x".repeat(pad)
    )
}

fn good_registry() -> (String, Seen) {
    spawn_stub(|path| {
        if path.contains("/blobs/") {
            ok(CONFIG)
        } else {
            ok(manifest(CONFIG.len(), 0))
        }
    })
}

#[tokio::test]
async fn a_well_formed_registry_is_accepted() {
    let (url, _) = good_registry();
    let variants = OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .unwrap();
    assert_eq!(variants.len(), GEMMA4_TAGS.len());
}

#[tokio::test]
async fn an_oversized_manifest_is_rejected() {
    let (url, _) = spawn_stub(|_| ok(manifest(CONFIG.len(), 300_000)));
    let err = OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .unwrap_err();
    assert!(err.to_string().contains("exceeds"), "{err}");
}

#[tokio::test]
async fn an_oversized_config_blob_is_rejected() {
    let big = format!(r#"{{"model_type":"7.5B","pad":"{}"}}"#, "x".repeat(100_000));
    let size = big.len();
    let (url, _) = spawn_stub(move |path| {
        if path.contains("/blobs/") {
            ok(big.clone())
        } else {
            ok(manifest(size, 0))
        }
    });
    let err = OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .unwrap_err();
    assert!(err.to_string().contains("exceeds"), "{err}");
}

#[tokio::test]
async fn a_redirect_is_not_followed() {
    let (target, target_seen) = good_registry();
    let location = format!("{target}/v2/library/gemma4/manifests/e4b");
    let (url, _) = spawn_stub(move |_| Reply {
        status: 302,
        headers: vec![("location".into(), location.clone())],
        body: Vec::new(),
        unannounced_length: false,
        split_at: None,
    });
    let err = OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("status"), "{err}");
    assert!(target_seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_plain_http_non_loopback_registry_is_refused_before_any_request() {
    for url in [
        "http://registry.example.invalid",
        "http://10.0.0.5:11434",
        "ftp://x",
    ] {
        let err = OllamaRegistryCatalog::new(url)
            .gemma4_variants()
            .await
            .unwrap_err();
        assert!(err.to_string().contains("https"), "{url}: {err}");
    }
}

#[tokio::test]
async fn a_config_blob_whose_length_differs_from_the_manifest_is_rejected() {
    let (url, _) = spawn_stub(|path| {
        if path.contains("/blobs/") {
            ok(CONFIG)
        } else {
            ok(manifest(CONFIG.len() + 1, 0))
        }
    });
    assert!(OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .is_err());
}

#[tokio::test]
async fn a_config_digest_that_is_not_plain_hex_is_never_requested() {
    let bad_digest = r#"{"config":{"digest":"sha256:../../x","size":1},"layers":[{"mediaType":"application/vnd.ollama.image.model","size":1000}]}"#;
    let (url, seen) = spawn_stub(move |_| ok(bad_digest));
    assert!(OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .is_err());
    assert!(seen.lock().unwrap().iter().all(|p| !p.contains("/blobs/")));
}

#[test]
fn a_manifest_without_a_model_layer_is_rejected_even_with_other_weights() {
    let projector_only =
        r#"{"layers":[{"mediaType":"application/vnd.ollama.image.projector","size":999}]}"#;
    assert!(variant_from_registry("gemma4:x", projector_only, CONFIG).is_err());
}

#[cfg(target_os = "linux")]
static SMI_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(target_os = "linux")]
fn smi_lock() -> std::sync::MutexGuard<'static, ()> {
    SMI_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(target_os = "linux")]
fn script_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "nanna-1064-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[cfg(target_os = "linux")]
fn fake_smi(body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let path = script_dir().join("nvidia-smi");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path.to_string_lossy().into_owned()
}

#[cfg(target_os = "linux")]
#[test]
fn a_working_nvidia_smi_gives_cuda_capacity() {
    let _guard = smi_lock();
    let smi = fake_smi("echo 8192");
    let cap = probe_device_with(&smi, Duration::from_secs(5)).unwrap();
    assert_eq!(cap.accel, Accel::Cuda);
    assert_eq!(cap.vram_bytes, Some(8192 << 20));
}

#[cfg(target_os = "linux")]
#[test]
fn a_hung_nvidia_smi_is_killed_and_refused_promptly() {
    let _guard = smi_lock();
    let smi = fake_smi("exec sleep 30");
    let started = Instant::now();
    let err = probe_device_with(&smi, Duration::from_millis(300)).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(err.to_string().contains("nvidia-smi"), "{err}");
}

#[cfg(target_os = "linux")]
#[test]
fn an_absent_nvidia_smi_is_not_an_error() {
    let _guard = smi_lock();
    let cap = probe_device_with("/nonexistent/nvidia-smi", Duration::from_secs(1)).unwrap();
    assert_ne!(cap.accel, Accel::Cuda);
}

#[cfg(target_os = "linux")]
#[test]
fn a_failing_nvidia_smi_counts_as_no_nvidia_gpu() {
    let _guard = smi_lock();
    let smi = fake_smi("exit 9");
    let cap = probe_device_with(&smi, Duration::from_secs(5)).unwrap();
    assert_ne!(cap.accel, Accel::Cuda);
}

fn auto(tag: &str) -> DefaultModel {
    DefaultModel::AutoGemma4(ModelVariant {
        tag: tag.into(),
        params: 1,
        min_memory_bytes: 1,
    })
}

fn runtime(names: &[&str]) -> OllamaProvider {
    let models: Vec<String> = names
        .iter()
        .map(|n| format!(r#"{{"name":"{n}","modified_at":"2024-01-01T00:00:00Z","size":1}}"#))
        .collect();
    let body = format!(r#"{{"models":[{}]}}"#, models.join(","));
    let (url, _) = spawn_stub(move |_| ok(body.clone()));
    OllamaProvider::new(OllamaConfig::default().with_base_url(url)).unwrap()
}

fn dead_runtime() -> OllamaProvider {
    OllamaProvider::new(OllamaConfig::default().with_base_url("http://127.0.0.1:1")).unwrap()
}

#[tokio::test]
async fn a_pulled_auto_selected_model_is_runnable() {
    ensure_runnable(&runtime(&["qwen3:0.6b", "gemma4:31b"]), &auto("gemma4:31b"))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_missing_auto_selected_model_is_refused_with_the_pull_command() {
    let err = ensure_runnable(&runtime(&["gemma4:e4b"]), &auto("gemma4:31b"))
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("ollama pull gemma4:31b"), "{text}");
    assert!(text.contains("--model"), "{text}");
}

#[tokio::test]
async fn a_tag_prefix_is_not_a_match() {
    assert!(
        ensure_runnable(&runtime(&["gemma4:31b-mlx"]), &auto("gemma4:31b"))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn an_unreachable_runtime_refuses_an_auto_selection() {
    let err = ensure_runnable(&dead_runtime(), &auto("gemma4:e4b"))
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("unreachable"), "{text}");
    assert!(text.contains("ollama pull gemma4:e4b"), "{text}");
}

#[tokio::test]
async fn a_configured_model_is_not_checked_against_the_local_runtime() {
    let configured = DefaultModel::Configured(ModelConfig {
        name: "hosted-api-model".into(),
    });
    ensure_runnable(&dead_runtime(), &configured).await.unwrap();
}

#[tokio::test]
async fn an_oversized_manifest_without_a_declared_length_is_cut_off_while_streaming() {
    let (url, _) = spawn_stub(|_| Reply {
        unannounced_length: true,
        ..ok(manifest(CONFIG.len(), 300_000))
    });
    let err = OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .unwrap_err();
    assert!(err.to_string().contains("exceeds"), "{err}");
}

#[tokio::test]
async fn an_oversized_config_blob_without_a_declared_length_is_cut_off_while_streaming() {
    let big = format!(r#"{{"model_type":"7.5B","pad":"{}"}}"#, "x".repeat(100_000));
    let size = big.len();
    let (url, _) = spawn_stub(move |path| {
        if path.contains("/blobs/") {
            Reply {
                unannounced_length: true,
                ..ok(big.clone())
            }
        } else {
            ok(manifest(size, 0))
        }
    });
    let err = OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .unwrap_err();
    assert!(err.to_string().contains("exceeds"), "{err}");
}

fn padded_config(total: usize) -> String {
    let base = r#"{"model_type":"7.5B","pad":""}"#.len();
    format!(
        r#"{{"model_type":"7.5B","pad":"{}"}}"#,
        "x".repeat(total - base)
    )
}

fn padded_manifest(config_size: usize, total: usize) -> String {
    let base = manifest(config_size, 0).len();
    manifest(config_size, total - base)
}

#[derive(Clone, Copy, Debug)]
enum Target {
    Manifest,
    Config,
}

async fn fetch_with_body_of(
    target: Target,
    length: usize,
    announce: bool,
    split_at: Option<usize>,
) -> Result<Vec<ModelVariant>, ModelError> {
    let (manifest_body, config_body) = match target {
        Target::Manifest => (padded_manifest(CONFIG.len(), length), CONFIG.to_string()),
        Target::Config => {
            let config = padded_config(length);
            (manifest(config.len(), 0), config)
        }
    };
    let (url, _) = spawn_stub(move |path| {
        let (body, wanted) = if path.contains("/blobs/") {
            (config_body.clone(), matches!(target, Target::Config))
        } else {
            (manifest_body.clone(), matches!(target, Target::Manifest))
        };
        Reply {
            unannounced_length: !announce && wanted,
            split_at: split_at.filter(|_| wanted),
            ..ok(body)
        }
    });
    OllamaRegistryCatalog::new(url).gemma4_variants().await
}

fn cap_of(target: Target) -> usize {
    match target {
        Target::Manifest => MAX_MANIFEST_BYTES,
        Target::Config => MAX_CONFIG_BYTES,
    }
}

#[tokio::test]
async fn a_body_of_exactly_the_cap_is_accepted() {
    for target in [Target::Manifest, Target::Config] {
        let cap = cap_of(target);
        for announce in [true, false] {
            for split in [None, Some(cap - 7)] {
                let got = fetch_with_body_of(target, cap, announce, split).await;
                assert!(got.is_ok(), "{target:?} {announce} {split:?}: {got:?}");
            }
        }
    }
}

#[tokio::test]
async fn a_body_one_byte_over_the_cap_is_rejected() {
    for target in [Target::Manifest, Target::Config] {
        let cap = cap_of(target);
        for announce in [true, false] {
            for split in [None, Some(cap - 7), Some(cap)] {
                let err = fetch_with_body_of(target, cap + 1, announce, split)
                    .await
                    .unwrap_err();
                let said_exceeds = err.to_string().contains("exceeds");
                assert!(said_exceeds, "{target:?} {announce} {split:?}: {err}");
            }
        }
    }
}

fn redirect_to(location: String) -> Reply {
    Reply {
        status: 307,
        headers: vec![("location".into(), location)],
        body: Vec::new(),
        unannounced_length: false,
        split_at: None,
    }
}

fn registry_redirecting_blobs_to(target: String, config_size: usize) -> String {
    let (url, _) = spawn_stub(move |path| {
        if path.contains("/blobs/") {
            redirect_to(target.clone())
        } else {
            ok(manifest(config_size, 0))
        }
    });
    url
}

#[tokio::test]
async fn a_config_blob_redirect_to_a_storage_host_is_followed_once() {
    let (storage, seen) = spawn_stub(|_| ok(CONFIG));
    let url = registry_redirecting_blobs_to(format!("{storage}/data?sig=1"), CONFIG.len());
    let variants = OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .unwrap();
    assert_eq!(variants.len(), GEMMA4_TAGS.len());
    assert!(seen.lock().unwrap().iter().all(|p| p.starts_with("/data")));
}

#[tokio::test]
async fn a_config_blob_redirect_to_plain_http_on_another_host_is_refused() {
    let url =
        registry_redirecting_blobs_to("http://storage.example.invalid/data".into(), CONFIG.len());
    let err = OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .unwrap_err();
    assert!(err.to_string().contains("redirect"), "{err}");
}

#[tokio::test]
async fn a_second_redirect_is_not_followed() {
    let (last, last_seen) = spawn_stub(|_| ok(CONFIG));
    let hop = format!("{last}/final");
    let (storage, _) = spawn_stub(move |_| redirect_to(hop.clone()));
    let url = registry_redirecting_blobs_to(format!("{storage}/data"), CONFIG.len());
    assert!(OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .is_err());
    assert!(last_seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_redirected_config_blob_is_still_size_capped_and_length_checked() {
    let big = padded_config(MAX_CONFIG_BYTES + 1);
    let (storage, _) = spawn_stub(move |_| ok(big.clone()));
    let url = registry_redirecting_blobs_to(format!("{storage}/data"), MAX_CONFIG_BYTES + 1);
    let err = OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .unwrap_err();
    assert!(err.to_string().contains("exceeds"), "{err}");

    let (storage, _) = spawn_stub(|_| ok(CONFIG));
    let url = registry_redirecting_blobs_to(format!("{storage}/data"), CONFIG.len() + 1);
    assert!(OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .is_err());
}

#[tokio::test]
async fn a_manifest_redirect_is_still_never_followed() {
    let (target, seen) = good_registry();
    let url = {
        let location = format!("{target}/v2/library/gemma4/manifests/e4b");
        let (url, _) = spawn_stub(move |_| redirect_to(location.clone()));
        url
    };
    assert!(OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .is_err());
    assert!(seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_config_blob_redirect_to_an_ip_literal_is_refused_before_any_request() {
    let url = registry_redirecting_blobs_to("https://127.0.0.1/data".into(), CONFIG.len());
    let err = OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .unwrap_err();
    assert!(err.to_string().contains("IP address"), "{err}");
}

#[tokio::test]
async fn a_manifest_that_is_not_utf8_is_rejected() {
    let (url, _) = spawn_stub(|_| ok(vec![0xff, 0xfe, 0xfd]));
    let err = OllamaRegistryCatalog::new(url)
        .gemma4_variants()
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not UTF-8"), "{err}");
}

#[cfg(target_os = "linux")]
#[test]
fn an_nvidia_smi_that_cannot_be_spawned_for_another_reason_is_a_probe_error() {
    let _guard = smi_lock();
    let err = probe_device_with("nvidia\0smi", Duration::from_secs(1)).unwrap_err();
    assert!(err.to_string().contains("cannot run"), "{err}");
}

#[cfg(target_os = "linux")]
#[test]
fn the_default_probe_reads_this_machine() {
    let _guard = smi_lock();
    let cap = probe_device().unwrap();
    assert!(cap.ram_bytes > 0);
}

#[test]
fn probe_errors_name_the_action_the_program_and_the_platform() {
    let io = std::io::Error::other("boom");
    let text = smi_error("nvidia-smi", "wait for", &io).to_string();
    let names_action = text.contains("cannot wait for nvidia-smi") && text.contains("boom");
    assert!(names_action, "{text}");
    let text = meminfo_error(&io).to_string();
    let names_platform = text.contains(std::env::consts::OS) && text.contains("/proc/meminfo");
    assert!(names_platform, "{text}");
}

fn sysfs_card(root: &std::path::Path, name: &str, vendor: &str, vram: Option<&str>) {
    let dir = root.join(name).join("device");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("vendor"), vendor).unwrap();
    if let Some(vram) = vram {
        std::fs::write(dir.join("mem_info_vram_total"), vram).unwrap();
    }
}

fn fresh_dir() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "nanna-1064-sysfs-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn the_largest_amd_vram_is_reported_and_other_vendors_and_bad_values_are_ignored() {
    let root = fresh_dir();
    sysfs_card(&root, "card0", "0x1002\n", Some("8589934592\n"));
    sysfs_card(&root, "card1", "0x1002\n", Some("17179869184\n"));
    sysfs_card(&root, "card2", "0x10de\n", Some("34359738368\n"));
    sysfs_card(&root, "card3", "0x1002\n", Some("0\n"));
    sysfs_card(&root, "card4", "0x1002\n", Some("lots\n"));
    sysfs_card(&root, "card5", "0x1002\n", None);
    assert_eq!(amd_vram_under(&root), Some(17_179_869_184));
}

#[test]
fn no_amd_card_or_no_sysfs_means_no_amd_vram() {
    let root = fresh_dir();
    sysfs_card(&root, "card0", "0x10de\n", Some("1024\n"));
    assert_eq!(amd_vram_under(&root), None);
    assert_eq!(amd_vram_under(&root.join("missing")), None);
}

#[test]
fn an_amd_card_that_reports_zero_or_unreadable_vram_is_not_a_gpu() {
    let zero = fresh_dir();
    sysfs_card(&zero, "card0", "0x1002\n", Some("0\n"));
    assert_eq!(amd_vram_under(&zero), None);
    let garbage = fresh_dir();
    sysfs_card(&garbage, "card0", "0x1002\n", Some("lots\n"));
    assert_eq!(amd_vram_under(&garbage), None);
}

#[test]
fn a_manifest_without_a_layers_array_is_rejected_with_a_clear_reason() {
    let err = variant_from_registry("gemma4:x", "{}", CONFIG).unwrap_err();
    assert!(err.to_string().contains("no layers"), "{err}");
}
