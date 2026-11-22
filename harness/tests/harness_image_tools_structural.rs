use std::path::Path;

fn definition_in(file: &str, start_marker: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(file);
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{} is readable: {e}", path.display()));
    let start = source
        .find(start_marker)
        .unwrap_or_else(|| panic!("{file} defines {start_marker}"));
    let rest = &source[start..];
    let end = rest.find("config = {").unwrap_or_else(|| {
        panic!("{start_marker} in {file} declares its config after its contents")
    });
    rest[..end].to_string()
}

fn harness_image_definitions() -> Vec<(&'static str, String)> {
    vec![
        (
            "nix/containers.nix",
            definition_in("nix/containers.nix", "harnessImage = "),
        ),
        (
            "flake.nix",
            definition_in("flake.nix", "harnessImage = if pkgs.stdenv.isLinux"),
        ),
    ]
}

#[test]
fn every_harness_image_definition_ships_the_git_binary_that_scope_and_workspace_shell_out_to() {
    for (file, definition) in harness_image_definitions() {
        assert!(
            definition.contains("pkgs.git"),
            "harnessImage in {file} must install pkgs.git: origin_slug and worktree creation run `git` inside the container, so an image without it refuses every repository as outside scope\n{definition}"
        );
    }
}

#[test]
fn every_harness_image_definition_keeps_its_runtime_basics() {
    for (file, definition) in harness_image_definitions() {
        for needed in ["pkgs.cacert", "pkgs.coreutils", "pkgs.bash"] {
            assert!(
                definition.contains(needed),
                "harnessImage in {file} lost {needed}"
            );
        }
    }
}

#[test]
fn every_harness_image_definition_ships_the_light_build_toolchain() {
    for (file, definition) in harness_image_definitions() {
        for needed in ["harnessToolchain", "pkgs.stdenv.cc"] {
            assert!(
                definition.contains(needed),
                "harnessImage in {file} must install {needed}: the cargo_check/cargo_test/cargo_build capability runs cargo and links with a C toolchain inside the image\n{definition}"
            );
        }
    }
}

#[test]
fn both_harness_image_definitions_pin_the_same_minimal_toolchain_expression() {
    let toolchains: Vec<String> = ["nix/containers.nix", "flake.nix"]
        .iter()
        .map(|file| {
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(file);
            let source = std::fs::read_to_string(&path).unwrap();
            let line = source
                .lines()
                .find(|l| l.trim_start().starts_with("harnessToolchain = "))
                .unwrap_or_else(|| panic!("{file} defines harnessToolchain"));
            line.trim().to_string()
        })
        .collect();
    assert_eq!(
        toolchains[0], toolchains[1],
        "nix/containers.nix and flake.nix must define harnessToolchain identically"
    );
    let version = toolchains[0].split('"').nth(1).unwrap_or_else(|| {
        panic!(
            "harnessToolchain pins a quoted stable version: {}",
            toolchains[0]
        )
    });
    let minor: u32 = version
        .split('.')
        .nth(1)
        .and_then(|m| m.parse().ok())
        .unwrap_or_else(|| panic!("harnessToolchain version is not x.y.z: {version}"));
    assert!(
        version.starts_with("1.") && minor >= 88,
        "marigold's vendored dependency set declares rust-version 1.88, so the bundled toolchain must be at least 1.88: {version}"
    );
    assert!(
        toolchains[0].contains(".minimal"),
        "the harness image carries only the minimal toolchain profile: {}",
        toolchains[0]
    );
}

#[test]
fn the_default_harness_image_carries_no_heavy_capability_toolchain() {
    for (file, definition) in harness_image_definitions() {
        for heavy in ["kani", "aeneas", "charon", "lean", "elan"] {
            assert!(
                !definition.to_lowercase().contains(heavy),
                "harnessImage in {file} must not bundle {heavy}: heavy capability toolchains belong in their own sandbox image"
            );
        }
    }
}

#[test]
fn every_nanna_build_that_feeds_the_harness_image_also_builds_the_sandbox_helper() {
    for file in ["flake.nix", "nix/packages.nix"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(file);
        let source = std::fs::read_to_string(&path).unwrap();
        assert!(
            source.contains("--bin nanna --bin nanna-sandbox"),
            "{file} must build nanna-sandbox next to nanna"
        );
        assert!(
            source.contains("cp target/release/nanna-sandbox $out/bin/"),
            "{file} must install nanna-sandbox next to nanna"
        );
    }
}
