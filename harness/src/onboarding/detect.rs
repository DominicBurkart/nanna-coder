use super::OnboardingError;
use crate::onboarding::profile::{
    BuildSystem, ProjectProfile, ToolCategory, ToolSpec, DEFAULT_RUST_VERSION,
};
use std::path::Path;

pub struct CargoManifest {
    pub name: String,
    pub edition: Option<String>,
    pub rust_version: Option<String>,
    pub is_workspace: bool,
    pub members: Vec<String>,
    pub dependencies: Vec<String>,
}

pub struct ProjectSignals {
    pub cargo_toml: Option<CargoManifest>,
    pub has_build_file: bool,
    pub has_makefile: bool,
    pub has_flake_nix: bool,
    pub top_level_entries: Vec<String>,
}

pub fn scan_project(source: &Path) -> Result<ProjectSignals, OnboardingError> {
    let top_level_entries: Vec<String> = std::fs::read_dir(source)
        .map_err(OnboardingError::Io)?
        .filter_map(|entry| entry.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();

    let has_build_file = top_level_entries
        .iter()
        .any(|name| name == "BUILD" || name == "BUILD.bazel");

    let has_makefile = top_level_entries
        .iter()
        .any(|name| name == "Makefile" || name == "makefile" || name == "justfile");

    let has_flake_nix = top_level_entries.iter().any(|name| name == "flake.nix");

    let cargo_toml = if top_level_entries.iter().any(|n| n == "Cargo.toml") {
        Some(parse_cargo_toml(&source.join("Cargo.toml"), source)?)
    } else {
        None
    };

    Ok(ProjectSignals {
        cargo_toml,
        has_build_file,
        has_makefile,
        has_flake_nix,
        top_level_entries,
    })
}

fn parse_cargo_toml(path: &Path, repo_root: &Path) -> Result<CargoManifest, OnboardingError> {
    let content = std::fs::read_to_string(path).map_err(OnboardingError::Io)?;
    let doc: toml::Value = content
        .parse()
        .map_err(|e| OnboardingError::ParseError(format!("invalid Cargo.toml: {}", e)))?;

    let is_workspace = doc.get("workspace").is_some();

    let dir_name = repo_root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("workspace");

    let name = if is_workspace {
        doc.get("workspace")
            .and_then(|w| w.get("package"))
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or(dir_name)
            .to_string()
    } else {
        doc.get("package")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .ok_or_else(|| {
                OnboardingError::ParseError("Cargo.toml missing [package].name".to_string())
            })?
            .to_string()
    };

    let edition = if is_workspace {
        doc.get("workspace")
            .and_then(|w| w.get("package"))
            .and_then(|p| p.get("edition"))
            .and_then(|e| e.as_str())
            .map(String::from)
    } else {
        doc.get("package")
            .and_then(|p| p.get("edition"))
            .and_then(|e| e.as_str())
            .map(String::from)
    };

    let rust_version = if is_workspace {
        doc.get("workspace")
            .and_then(|w| w.get("package"))
            .and_then(|p| p.get("rust-version"))
            .and_then(|v| v.as_str())
            .map(String::from)
    } else {
        doc.get("package")
            .and_then(|p| p.get("rust-version"))
            .and_then(|v| v.as_str())
            .map(String::from)
    };

    let members = if is_workspace {
        doc.get("workspace")
            .and_then(|w| w.get("members"))
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        vec![]
    };

    let deps_table = if is_workspace {
        doc.get("workspace").and_then(|w| w.get("dependencies"))
    } else {
        doc.get("dependencies")
    };

    let mut dependencies: std::collections::HashSet<String> = deps_table
        .and_then(|d| d.as_table())
        .map(|t| t.keys().cloned().collect())
        .unwrap_or_default();

    if is_workspace {
        for member in &members {
            let member_cargo = repo_root.join(member).join("Cargo.toml");
            if let Ok(member_content) = std::fs::read_to_string(&member_cargo) {
                if let Ok(member_doc) = member_content.parse::<toml::Value>() {
                    if let Some(member_deps) =
                        member_doc.get("dependencies").and_then(|d| d.as_table())
                    {
                        for key in member_deps.keys() {
                            dependencies.insert(key.clone());
                        }
                    }
                }
            }
        }
    }

    Ok(CargoManifest {
        name,
        edition,
        rust_version,
        is_workspace,
        members,
        dependencies: dependencies.into_iter().collect(),
    })
}

impl ProjectSignals {
    pub fn to_cargo_profile(&self) -> Result<ProjectProfile, OnboardingError> {
        let manifest = self
            .cargo_toml
            .as_ref()
            .ok_or(OnboardingError::NotCargoProject)?;

        let mut nix_packages = vec![
            "rustToolchain".to_string(),
            "pkgs.cargo-nextest".to_string(),
            "pkgs.bash".to_string(),
            "pkgs.coreutils".to_string(),
            "pkgs.git".to_string(),
            "pkgs.cacert".to_string(),
        ];

        if manifest.dependencies.iter().any(|d| d == "openssl") {
            nix_packages.push("pkgs.pkg-config".to_string());
            nix_packages.push("pkgs.openssl".to_string());
        }

        let tools = vec![
            ToolSpec::new(
                "build",
                "cargo build",
                "Build the project",
                ToolCategory::Build,
            )
            .map_err(|e| OnboardingError::ProfileError(e.to_string()))?,
            ToolSpec::new("test", "cargo test", "Run tests", ToolCategory::Test)
                .map_err(|e| OnboardingError::ProfileError(e.to_string()))?,
            ToolSpec::new(
                "clippy",
                "cargo clippy -- -D warnings",
                "Run clippy linter",
                ToolCategory::Lint,
            )
            .map_err(|e| OnboardingError::ProfileError(e.to_string()))?,
            ToolSpec::new(
                "fmt-check",
                "cargo fmt --check",
                "Check formatting",
                ToolCategory::Format,
            )
            .map_err(|e| OnboardingError::ProfileError(e.to_string()))?,
        ];

        let rust_version = manifest
            .rust_version
            .clone()
            .unwrap_or_else(|| DEFAULT_RUST_VERSION.to_string());

        Ok(ProjectProfile {
            project_name: manifest.name.clone(),
            build_system: BuildSystem::Cargo,
            tools,
            nix_packages,
            rust_version: Some(rust_version),
            extra_env_vars: vec![],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write_file(dir: &TempDir, name: &str, content: &str) {
        fs::write(dir.path().join(name), content).unwrap();
    }

    #[test]
    fn scan_empty_dir_returns_empty_signals() {
        let dir = TempDir::new().unwrap();
        let signals = scan_project(dir.path()).unwrap();
        assert!(signals.cargo_toml.is_none());
        assert!(!signals.has_build_file);
        assert!(!signals.has_makefile);
        assert!(!signals.has_flake_nix);
        assert!(signals.top_level_entries.is_empty());
    }

    #[test]
    fn scan_detects_cargo_toml() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "Cargo.toml",
            r#"
[package]
name = "myproject"
version = "0.1.0"
edition = "2021"
"#,
        );
        let signals = scan_project(dir.path()).unwrap();
        let manifest = signals.cargo_toml.unwrap();
        assert_eq!(manifest.name, "myproject");
        assert_eq!(manifest.edition, Some("2021".to_string()));
        assert!(!manifest.is_workspace);
        assert!(manifest.members.is_empty());
    }

    #[test]
    fn scan_detects_build_file() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "BUILD", "");
        let signals = scan_project(dir.path()).unwrap();
        assert!(signals.has_build_file);
    }

    #[test]
    fn scan_detects_build_bazel() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "BUILD.bazel", "");
        let signals = scan_project(dir.path()).unwrap();
        assert!(signals.has_build_file);
    }

    #[test]
    fn scan_detects_flake_nix() {
        let dir = TempDir::new().unwrap();
        write_file(&dir, "flake.nix", "{}");
        let signals = scan_project(dir.path()).unwrap();
        assert!(signals.has_flake_nix);
    }

    #[test]
    fn scan_cargo_with_build_file_has_both() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "Cargo.toml",
            r#"
[package]
name = "mixed"
version = "0.1.0"
"#,
        );
        write_file(&dir, "BUILD", "");
        let signals = scan_project(dir.path()).unwrap();
        assert!(signals.cargo_toml.is_some());
        assert!(signals.has_build_file);
    }

    #[test]
    fn scan_workspace_cargo_toml() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "Cargo.toml",
            r#"
[workspace]
members = ["crate-a", "crate-b"]

[workspace.package]
name = "myworkspace"
version = "0.1.0"
"#,
        );
        let signals = scan_project(dir.path()).unwrap();
        let manifest = signals.cargo_toml.unwrap();
        assert!(manifest.is_workspace);
        assert_eq!(manifest.members, vec!["crate-a", "crate-b"]);
    }

    #[test]
    fn to_cargo_profile_basic() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "Cargo.toml",
            r#"
[package]
name = "myapp"
version = "0.1.0"
edition = "2021"
"#,
        );
        let signals = scan_project(dir.path()).unwrap();
        let profile = signals.to_cargo_profile().unwrap();
        assert_eq!(profile.project_name, "myapp");
        assert_eq!(profile.build_system, BuildSystem::Cargo);
        assert_eq!(profile.rust_version, Some(DEFAULT_RUST_VERSION.to_string()));
        assert!(profile.nix_packages.contains(&"rustToolchain".to_string()));
        assert!(profile
            .nix_packages
            .contains(&"pkgs.cargo-nextest".to_string()));
        assert!(profile.nix_packages.contains(&"pkgs.bash".to_string()));
        assert!(profile.nix_packages.contains(&"pkgs.coreutils".to_string()));
        assert!(profile.nix_packages.contains(&"pkgs.git".to_string()));
        assert!(profile.nix_packages.contains(&"pkgs.cacert".to_string()));
        assert!(!profile.nix_packages.contains(&"pkgs.openssl".to_string()));
        assert_eq!(profile.tools.len(), 4);
    }

    #[test]
    fn to_cargo_profile_adds_openssl_packages() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "Cargo.toml",
            r#"
[package]
name = "tlsapp"
version = "0.1.0"

[dependencies]
openssl = "0.10"
"#,
        );
        let signals = scan_project(dir.path()).unwrap();
        let profile = signals.to_cargo_profile().unwrap();
        assert!(profile
            .nix_packages
            .contains(&"pkgs.pkg-config".to_string()));
        assert!(profile.nix_packages.contains(&"pkgs.openssl".to_string()));
    }

    #[test]
    fn to_cargo_profile_fails_without_cargo_toml() {
        let dir = TempDir::new().unwrap();
        let signals = scan_project(dir.path()).unwrap();
        assert!(matches!(
            signals.to_cargo_profile(),
            Err(OnboardingError::NotCargoProject)
        ));
    }

    #[test]
    fn workspace_member_openssl_dep_is_collected() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "Cargo.toml",
            r#"
[workspace]
members = ["member-a"]
"#,
        );
        fs::create_dir(dir.path().join("member-a")).unwrap();
        fs::write(
            dir.path().join("member-a").join("Cargo.toml"),
            r#"
[package]
name = "member-a"
version = "0.1.0"

[dependencies]
openssl = "0.10"
"#,
        )
        .unwrap();
        let signals = scan_project(dir.path()).unwrap();
        let manifest = signals.cargo_toml.unwrap();
        assert!(manifest.dependencies.contains(&"openssl".to_string()));
    }

    #[test]
    fn workspace_member_openssl_dep_adds_nix_packages() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "Cargo.toml",
            r#"
[workspace]
members = ["member-a"]
"#,
        );
        fs::create_dir(dir.path().join("member-a")).unwrap();
        fs::write(
            dir.path().join("member-a").join("Cargo.toml"),
            r#"
[package]
name = "member-a"
version = "0.1.0"

[dependencies]
openssl = "0.10"
"#,
        )
        .unwrap();
        let signals = scan_project(dir.path()).unwrap();
        let profile = signals.to_cargo_profile().unwrap();
        assert!(profile.nix_packages.contains(&"pkgs.openssl".to_string()));
        assert!(profile
            .nix_packages
            .contains(&"pkgs.pkg-config".to_string()));
    }

    #[test]
    fn to_cargo_profile_reads_rust_version_from_cargo_toml() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "Cargo.toml",
            r#"
[package]
name = "myapp"
version = "0.1.0"
rust-version = "1.80.0"
"#,
        );
        let signals = scan_project(dir.path()).unwrap();
        let profile = signals.to_cargo_profile().unwrap();
        assert_eq!(profile.rust_version, Some("1.80.0".to_string()));
    }

    #[test]
    fn to_cargo_profile_reads_rust_version_from_workspace_cargo_toml() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "Cargo.toml",
            r#"
[workspace]
members = []

[workspace.package]
rust-version = "1.78.0"
"#,
        );
        let signals = scan_project(dir.path()).unwrap();
        let profile = signals.to_cargo_profile().unwrap();
        assert_eq!(profile.rust_version, Some("1.78.0".to_string()));
    }

    #[test]
    fn workspace_without_package_name_uses_dir_name() {
        let dir = TempDir::new().unwrap();
        write_file(
            &dir,
            "Cargo.toml",
            r#"
[workspace]
members = ["crate-a"]
"#,
        );
        let dir_name = dir
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let signals = scan_project(dir.path()).unwrap();
        let manifest = signals.cargo_toml.unwrap();
        assert_eq!(manifest.name, dir_name);
    }
}
