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
