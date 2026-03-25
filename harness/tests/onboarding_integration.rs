use harness::onboarding::{DeterministicOnboarder, Onboarder};
use std::fs;
use tempfile::TempDir;

fn write_file(dir: &TempDir, name: &str, content: &str) {
    fs::write(dir.path().join(name), content).unwrap();
}

fn minimal_cargo_toml(name: &str) -> String {
    format!(
        r#"[package]
name = "{}"
version = "0.1.0"
edition = "2021"
"#,
        name
    )
}

fn minimal_main_rs() -> &'static str {
    "fn main() {}"
}

#[tokio::test]
async fn onboard_minimal_rust_project_writes_flake_nix() {
    let dir = TempDir::new().unwrap();
    let src_dir = dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    write_file(&dir, "Cargo.toml", &minimal_cargo_toml("hello-world"));
    fs::write(src_dir.join("main.rs"), minimal_main_rs()).unwrap();

    let onboarder = DeterministicOnboarder;
    let result = onboarder.onboard(dir.path()).await.unwrap();

    assert!(
        result.flake_path.exists(),
        "flake.nix should have been written"
    );
    assert_eq!(result.profile.project_name, "hello-world");

    let flake_content = fs::read_to_string(&result.flake_path).unwrap();
    assert!(flake_content.contains("devContainerImage"));
    assert!(flake_content.contains("rust-overlay"));
    assert!(flake_content.contains("rustToolchain"));
    assert!(flake_content.contains(r#"Cmd = [ "sleep" "infinity" ]"#));
    assert!(flake_content.contains(r#"WorkingDir = "/workspace""#));
    assert!(flake_content.contains("devShells.default"));
    assert!(flake_content.contains("1.84.0"));
}

#[tokio::test]
async fn onboard_project_with_openssl_includes_ssl_packages() {
    let dir = TempDir::new().unwrap();
    let src_dir = dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    write_file(
        &dir,
        "Cargo.toml",
        r#"[package]
name = "tls-server"
version = "0.1.0"
edition = "2021"

[dependencies]
openssl = "0.10"
"#,
    );
    fs::write(src_dir.join("main.rs"), minimal_main_rs()).unwrap();

    let onboarder = DeterministicOnboarder;
    let result = onboarder.onboard(dir.path()).await.unwrap();

    let flake_content = fs::read_to_string(&result.flake_path).unwrap();
    assert!(flake_content.contains("pkgs.openssl"));
    assert!(flake_content.contains("pkgs.pkg-config"));
}

#[tokio::test]
async fn onboard_fails_when_flake_already_exists() {
    use harness::onboarding::OnboardingError;

    let dir = TempDir::new().unwrap();
    write_file(&dir, "flake.nix", "{}");
    write_file(&dir, "Cargo.toml", &minimal_cargo_toml("existing"));

    let onboarder = DeterministicOnboarder;
    let err = onboarder.onboard(dir.path()).await.unwrap_err();
    assert!(matches!(err, OnboardingError::AlreadyOnboarded));
}

#[tokio::test]
async fn onboard_fails_without_cargo_toml() {
    use harness::onboarding::OnboardingError;

    let dir = TempDir::new().unwrap();
    let onboarder = DeterministicOnboarder;
    let err = onboarder.onboard(dir.path()).await.unwrap_err();
    assert!(matches!(err, OnboardingError::NotCargoProject));
}

#[tokio::test]
async fn onboard_fails_with_ambiguous_build_system() {
    use harness::onboarding::OnboardingError;

    let dir = TempDir::new().unwrap();
    write_file(&dir, "Cargo.toml", &minimal_cargo_toml("mixed"));
    write_file(&dir, "BUILD", "");

    let onboarder = DeterministicOnboarder;
    let err = onboarder.onboard(dir.path()).await.unwrap_err();
    assert!(matches!(err, OnboardingError::AmbiguousBuildSystem));
}

#[tokio::test]
async fn onboard_generated_flake_matches_reference_structure() {
    let dir = TempDir::new().unwrap();
    let src_dir = dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    write_file(
        &dir,
        "Cargo.toml",
        r#"[package]
name = "fibonacci-example"
version = "0.1.0"
edition = "2021"

[dependencies]
openssl = "0.10"
"#,
    );
    fs::write(src_dir.join("main.rs"), minimal_main_rs()).unwrap();

    let onboarder = DeterministicOnboarder;
    let result = onboarder.onboard(dir.path()).await.unwrap();
    let flake = fs::read_to_string(&result.flake_path).unwrap();

    assert!(flake.contains("github:NixOS/nixpkgs/nixos-unstable"));
    assert!(flake.contains("github:numtide/flake-utils"));
    assert!(flake.contains("github:oxalica/rust-overlay"));
    assert!(flake.contains("github:nlewo/nix2container"));
    assert!(flake.contains("nix2container.buildImage"));
    assert!(flake.contains("maxLayers = 100"));
    assert!(flake.contains("pkgs.cargo-nextest"));
    assert!(flake.contains("pkgs.bash"));
    assert!(flake.contains("pkgs.coreutils"));
    assert!(flake.contains("pkgs.git"));
    assert!(flake.contains("pkgs.cacert"));
}
