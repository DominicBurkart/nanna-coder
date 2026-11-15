use async_trait::async_trait;
use model::default_model::*;
use model::{ChatRequest, ChatResponse, ModelError, ModelInfo, ModelProvider, ModelResult};
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

fn not_found() -> Reply {
    Reply {
        status: 404,
        headers: Vec::new(),
        body: b"{}".to_vec(),
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
            match reply.split_at {
                Some(at) if at < reply.body.len() => {
                    let _ = stream.write_all(&reply.body[..at]);
                    let _ = stream.flush();
                    std::thread::sleep(Duration::from_millis(100));
                    let _ = stream.write_all(&reply.body[at..]);
                }
                _ => {
                    let _ = stream.write_all(&reply.body);
                }
            }
        }
    });
    (format!("http://{addr}"), seen)
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
        } else if path.contains("/manifests/") {
            ok(manifest(CONFIG.len(), 0))
        } else {
            not_found()
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
    let (url, _) = spawn_stub(|path| {
        if path.contains("/blobs/") {
            ok(CONFIG)
        } else {
            ok(manifest(CONFIG.len(), 300_000))
        }
    });
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
    let (url, seen) = spawn_stub(|path| {
        if path.contains("/manifests/") {
            ok(
                r#"{"config":{"digest":"sha256:../../x","size":1},"layers":[{"mediaType":"application/vnd.ollama.image.model","size":1000}]}"#,
            )
        } else {
            ok(CONFIG)
        }
    });
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
    let smi = fake_smi("echo 8192");
    let cap = probe_device_with(&smi, Duration::from_secs(5)).unwrap();
    assert_eq!(cap.accel, Accel::Cuda);
    assert_eq!(cap.vram_bytes, Some(8192 << 20));
}

#[cfg(target_os = "linux")]
#[test]
fn a_hung_nvidia_smi_is_killed_and_refused_promptly() {
    let smi = fake_smi("exec sleep 30");
    let started = Instant::now();
    let err = probe_device_with(&smi, Duration::from_millis(300)).unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(err.to_string().contains("nvidia-smi"), "{err}");
}

#[cfg(target_os = "linux")]
#[test]
fn an_absent_nvidia_smi_is_not_an_error() {
    let cap = probe_device_with("/nonexistent/nvidia-smi", Duration::from_secs(1)).unwrap();
    assert_ne!(cap.accel, Accel::Cuda);
}

#[cfg(target_os = "linux")]
#[test]
fn a_failing_nvidia_smi_counts_as_no_nvidia_gpu() {
    let smi = fake_smi("exit 9");
    let cap = probe_device_with(&smi, Duration::from_secs(5)).unwrap();
    assert_ne!(cap.accel, Accel::Cuda);
}

struct Runtime {
    names: Option<Vec<String>>,
}

#[async_trait]
impl ModelProvider for Runtime {
    async fn chat(&self, _request: ChatRequest) -> ModelResult<ChatResponse> {
        Err(ModelError::Unknown {
            message: "unused".into(),
        })
    }
    async fn list_models(&self) -> ModelResult<Vec<ModelInfo>> {
        match &self.names {
            Some(names) => Ok(names
                .iter()
                .map(|n| ModelInfo {
                    name: n.clone(),
                    size: None,
                    digest: None,
                    modified_at: None,
                })
                .collect()),
            None => Err(ModelError::ServiceUnavailable {
                message: "down".into(),
            }),
        }
    }
    async fn health_check(&self) -> ModelResult<()> {
        Ok(())
    }
    fn provider_name(&self) -> &'static str {
        "fake"
    }
}

fn auto(tag: &str) -> DefaultModel {
    DefaultModel::AutoGemma4(ModelVariant {
        tag: tag.into(),
        params: 1,
        min_memory_bytes: 1,
    })
}

fn runtime(names: &[&str]) -> Runtime {
    Runtime {
        names: Some(names.iter().map(|n| n.to_string()).collect()),
    }
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
    let err = ensure_runnable(&Runtime { names: None }, &auto("gemma4:e4b"))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("ollama"), "{err}");
}

#[tokio::test]
async fn a_configured_model_is_not_checked_against_the_local_runtime() {
    let configured = DefaultModel::Configured(ModelConfig {
        name: "hosted-api-model".into(),
    });
    ensure_runnable(&Runtime { names: None }, &configured)
        .await
        .unwrap();
}

#[tokio::test]
async fn an_oversized_manifest_without_a_declared_length_is_cut_off_while_streaming() {
    let (url, _) = spawn_stub(|path| {
        if path.contains("/blobs/") {
            ok(CONFIG)
        } else {
            Reply {
                unannounced_length: true,
                ..ok(manifest(CONFIG.len(), 300_000))
            }
        }
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
                assert!(
                    got.is_ok(),
                    "{target:?} announce={announce} split={split:?}: {got:?}"
                );
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
                assert!(
                    err.to_string().contains("exceeds"),
                    "{target:?} announce={announce} split={split:?}: {err}"
                );
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
