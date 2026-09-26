use super::OnboardingError;
use crate::onboarding::profile::{BuildSystem, ProjectProfile, DEFAULT_RUST_VERSION};

const CARGO_FLAKE_TEMPLATE: &str = r#"{
  description = "__PROJECT_NAME__ dev container";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    nix2container = {
      url = "github:nlewo/nix2container";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay, nix2container }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.stable."__RUST_VERSION__".default.override {
          extensions = [ "rust-src" "rustfmt" "clippy" "rust-analyzer" ];__RUST_TARGETS__
        };

        nix2containerPkgs = nix2container.packages.${system};

        devContainerPackages = [
          __PACKAGES__
        ];

        devContainerImage = nix2containerPkgs.nix2container.buildImage {
          name = "__PROJECT_NAME__-dev";
          tag = "latest";

          copyToRoot = pkgs.buildEnv {
            name = "dev-env";
            paths = devContainerPackages;
            pathsToLink = [ "/bin" "/etc" "/share" "/lib" "/include" ];
          };

          config = {
            Cmd = [ "sleep" "infinity" ];
            Env = [
              "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
              "PATH=/bin"
              "RUST_LOG=info"
            ];
            WorkingDir = "/workspace";
          };

          maxLayers = 100;
        };

      in
      {
        packages = {
          inherit devContainerImage;
        };

        devShells.default = pkgs.mkShell {
          buildInputs = devContainerPackages;
        };
      }
    );
}
"#;

pub fn generate_flake(profile: &ProjectProfile) -> Result<String, OnboardingError> {
    match profile.build_system {
        BuildSystem::Cargo => generate_cargo_flake(profile),
    }
}

fn generate_cargo_flake(profile: &ProjectProfile) -> Result<String, OnboardingError> {
    let rust_version = profile
        .rust_version
        .as_deref()
        .unwrap_or(DEFAULT_RUST_VERSION);

    let packages_str = profile.nix_packages.join("\n          ");
    let targets_str = render_rust_targets(&profile.rust_targets);

    let flake = CARGO_FLAKE_TEMPLATE
        .replace("__PROJECT_NAME__", &profile.project_name)
        .replace("__RUST_VERSION__", rust_version)
        .replace("__RUST_TARGETS__", &targets_str)
        .replace("__PACKAGES__", &packages_str);

    Ok(flake)
}

/// Render the `targets = [ ... ];` line of the toolchain override, or an
/// empty string when no extra targets are requested.
fn render_rust_targets(targets: &[String]) -> String {
    if targets.is_empty() {
        return String::new();
    }
    let quoted: Vec<String> = targets.iter().map(|t| format!("\"{t}\"")).collect();
    format!("\n          targets = [ {} ];", quoted.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboarding::profile::{
        BuildSystem, ProjectProfile, ToolCategory, ToolSpec, DEFAULT_RUST_VERSION,
    };

    fn minimal_cargo_profile(name: &str) -> ProjectProfile {
        ProjectProfile {
            project_name: name.to_string(),
            build_system: BuildSystem::Cargo,
            tools: vec![
                ToolSpec::new("build", "cargo build", "Build", ToolCategory::Build).unwrap(),
            ],
            nix_packages: vec![
                "rustToolchain".to_string(),
                "pkgs.cargo-nextest".to_string(),
                "pkgs.bash".to_string(),
                "pkgs.coreutils".to_string(),
                "pkgs.git".to_string(),
                "pkgs.cacert".to_string(),
            ],
            rust_version: Some(DEFAULT_RUST_VERSION.to_string()),
            rust_targets: vec![],
            extra_env_vars: vec![],
        }
    }

    #[test]
    fn generated_flake_omits_targets_line_by_default() {
        let profile = minimal_cargo_profile("myapp");
        let flake = generate_flake(&profile).unwrap();
        assert!(!flake.contains("targets ="));
        assert!(flake.contains(
            "extensions = [ \"rust-src\" \"rustfmt\" \"clippy\" \"rust-analyzer\" ];\n        };"
        ));
    }

    #[test]
    fn generated_flake_adds_rust_targets_to_toolchain_override() {
        let mut profile = minimal_cargo_profile("webapp");
        profile.rust_targets = vec!["wasm32-unknown-unknown".to_string()];
        let flake = generate_flake(&profile).unwrap();
        assert!(flake.contains(
            "extensions = [ \"rust-src\" \"rustfmt\" \"clippy\" \"rust-analyzer\" ];\n          targets = [ \"wasm32-unknown-unknown\" ];\n        };"
        ));
    }

    #[test]
    fn render_rust_targets_quotes_every_target() {
        assert_eq!(render_rust_targets(&[]), "");
        assert_eq!(
            render_rust_targets(&["a".to_string(), "b".to_string()]),
            "\n          targets = [ \"a\" \"b\" ];"
        );
    }

    #[test]
    fn generated_flake_contains_rust_overlay() {
        let profile = minimal_cargo_profile("myapp");
        let flake = generate_flake(&profile).unwrap();
        assert!(flake.contains("rust-overlay"));
    }

    #[test]
    fn generated_flake_contains_dev_container_image() {
        let profile = minimal_cargo_profile("myapp");
        let flake = generate_flake(&profile).unwrap();
        assert!(flake.contains("devContainerImage"));
    }

    #[test]
    fn generated_flake_contains_rust_toolchain() {
        let profile = minimal_cargo_profile("myapp");
        let flake = generate_flake(&profile).unwrap();
        assert!(flake.contains("rustToolchain"));
    }

    #[test]
    fn generated_flake_contains_base_packages() {
        let profile = minimal_cargo_profile("myapp");
        let flake = generate_flake(&profile).unwrap();
        assert!(flake.contains("pkgs.bash"));
        assert!(flake.contains("pkgs.coreutils"));
        assert!(flake.contains("pkgs.git"));
        assert!(flake.contains("pkgs.cacert"));
    }

    #[test]
    fn generated_flake_contains_sleep_infinity_cmd() {
        let profile = minimal_cargo_profile("myapp");
        let flake = generate_flake(&profile).unwrap();
        assert!(flake.contains(r#"Cmd = [ "sleep" "infinity" ]"#));
    }

    #[test]
    fn generated_flake_contains_workspace_dir() {
        let profile = minimal_cargo_profile("myapp");
        let flake = generate_flake(&profile).unwrap();
        assert!(flake.contains(r#"WorkingDir = "/workspace""#));
    }

    #[test]
    fn generated_flake_contains_dev_shells() {
        let profile = minimal_cargo_profile("myapp");
        let flake = generate_flake(&profile).unwrap();
        assert!(flake.contains("devShells.default"));
    }

    #[test]
    fn generated_flake_contains_extra_packages() {
        let mut profile = minimal_cargo_profile("tlsapp");
        profile.nix_packages.push("pkgs.pkg-config".to_string());
        profile.nix_packages.push("pkgs.openssl".to_string());
        let flake = generate_flake(&profile).unwrap();
        assert!(flake.contains("pkgs.pkg-config"));
        assert!(flake.contains("pkgs.openssl"));
    }

    #[test]
    fn generated_flake_uses_project_name() {
        let profile = minimal_cargo_profile("cool-project");
        let flake = generate_flake(&profile).unwrap();
        assert!(flake.contains("cool-project"));
        assert!(flake.contains("cool-project-dev"));
    }

    #[test]
    fn generated_flake_uses_rust_version() {
        let mut profile = minimal_cargo_profile("myapp");
        profile.rust_version = Some("1.75.0".to_string());
        let flake = generate_flake(&profile).unwrap();
        assert!(flake.contains("1.75.0"));
    }
}
