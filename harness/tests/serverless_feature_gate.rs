use std::fs;
use std::path::{Path, PathBuf};

fn harness_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    fs::read_to_string(harness_dir().join(relative)).unwrap()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn serverless_adapter_is_an_optional_feature_outside_the_defaults_with_no_dependencies() {
    let manifest: toml::Table = toml::from_str(&read("Cargo.toml")).unwrap();
    let features = manifest["features"].as_table().unwrap();
    let adapter = features["serverless-adapter"].as_array().unwrap();
    assert!(adapter.is_empty(), "{adapter:?}");
    if let Some(default) = features.get("default") {
        assert!(!default
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry.as_str() == Some("serverless-adapter")));
    }
    for (name, list) in features {
        if name != "serverless-adapter" {
            assert!(!list
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry.as_str() == Some("serverless-adapter")));
        }
    }
}

#[test]
fn core_dependencies_contain_no_cloud_provider_sdk() {
    let manifest: toml::Table = toml::from_str(&read("Cargo.toml")).unwrap();
    for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
        let Some(table) = manifest.get(section).and_then(toml::Value::as_table) else {
            continue;
        };
        for name in table.keys() {
            let lower = name.to_lowercase();
            for sdk in [
                "aws-",
                "aws_",
                "rusoto",
                "azure",
                "google-cloud",
                "gcloud",
                "scaleway",
            ] {
                assert!(!lower.contains(sdk), "{section} depends on `{name}`");
            }
        }
    }
}

#[test]
fn the_adapter_module_and_its_reexports_are_cfg_gated() {
    let source = read("src/rollout/mod.rs");
    let lines: Vec<&str> = source.lines().collect();
    let gate = "#[cfg(feature = \"serverless-adapter\")]";
    let mut gated_items = 0;
    for (index, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("mod serverless")
            || line.trim_start().starts_with("pub use serverless::")
        {
            assert!(
                index > 0 && lines[index - 1].trim() == gate,
                "line {} is not gated: {line}",
                index + 1
            );
            gated_items += 1;
        }
    }
    assert_eq!(gated_items, 2);
}

#[test]
fn nothing_outside_the_adapter_module_names_the_adapter() {
    let mut files = Vec::new();
    rust_files(&harness_dir().join("src"), &mut files);
    for file in files {
        let relative = file.strip_prefix(harness_dir()).unwrap().to_path_buf();
        if relative == Path::new("src/rollout/serverless.rs")
            || relative == Path::new("src/rollout/mod.rs")
        {
            continue;
        }
        let text = fs::read_to_string(&file).unwrap();
        for symbol in [
            "ServerlessAdapter",
            "ServerlessConfig",
            "rollout::serverless",
            "SERVERLESS_ENV",
        ] {
            assert!(
                !text.contains(symbol),
                "{} names `{symbol}`",
                relative.display()
            );
        }
    }
}
