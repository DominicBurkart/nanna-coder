use std::path::Path;

fn harness_image_definition() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../nix/containers.nix");
    let source = std::fs::read_to_string(&path).expect("nix/containers.nix is readable");
    let start = source
        .find("harnessImage = ")
        .expect("nix/containers.nix defines harnessImage");
    let rest = &source[start..];
    let end = rest
        .find("config = {")
        .expect("harnessImage declares its config after its contents");
    rest[..end].to_string()
}

#[test]
fn the_harness_image_ships_the_git_binary_that_scope_and_workspace_shell_out_to() {
    let definition = harness_image_definition();
    assert!(
        definition.contains("pkgs.git"),
        "harnessImage must install pkgs.git: origin_slug and worktree creation run `git` inside the container, so an image without it refuses every repository as outside scope\n{definition}"
    );
}

#[test]
fn the_harness_image_keeps_its_runtime_basics() {
    let definition = harness_image_definition();
    for needed in ["harness", "pkgs.cacert", "pkgs.coreutils", "pkgs.bash"] {
        assert!(definition.contains(needed), "harnessImage lost {needed}");
    }
}
