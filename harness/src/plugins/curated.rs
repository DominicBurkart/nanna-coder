use super::load::PluginSet;
use super::render::PluginSource;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

const STACKS: [(&str, &str); 2] = [
    (
        "rust-cargo",
        include_str!("../../../plugins/stacks/rust-cargo.toml"),
    ),
    ("nix", include_str!("../../../plugins/stacks/nix.toml")),
];

const ROLES: [(&str, &str); 5] = [
    (
        "implementer",
        include_str!("../../../plugins/roles/implementer.toml"),
    ),
    (
        "reviewer",
        include_str!("../../../plugins/roles/reviewer.toml"),
    ),
    (
        "researcher",
        include_str!("../../../plugins/roles/researcher.toml"),
    ),
    (
        "docs-writer",
        include_str!("../../../plugins/roles/docs-writer.toml"),
    ),
    (
        "pr-shepherd",
        include_str!("../../../plugins/roles/pr-shepherd.toml"),
    ),
];

const AGENTS: [(&str, &str); 1] = [(
    "rust-implementer",
    include_str!("../../../plugins/agents/rust-implementer.toml"),
)];

pub fn curated_set() -> PluginSet {
    let mut set = PluginSet::default();
    for (name, text) in STACKS {
        set.add_stack(
            name,
            text,
            PluginSource::Curated,
            &format!("curated:stacks/{name}"),
        )
        .unwrap_or_else(|e| panic!("embedded curated stack `{name}` is invalid: {e}"));
    }
    for (name, text) in ROLES {
        set.add_role(
            text,
            PluginSource::Curated,
            &format!("curated:roles/{name}"),
        )
        .unwrap_or_else(|e| panic!("embedded curated role `{name}` is invalid: {e}"));
    }
    for (name, text) in AGENTS {
        set.add_agent(
            text,
            PluginSource::Curated,
            &format!("curated:agents/{name}"),
        )
        .unwrap_or_else(|e| panic!("embedded curated agent `{name}` is invalid: {e}"));
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EffectClass;
    use crate::identity::AgentIdentity;

    #[test]
    fn every_embedded_plugin_parses_and_every_role_name_matches_its_file() {
        let set = curated_set();
        assert_eq!(
            set.roles.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "docs-writer",
                "implementer",
                "pr-shepherd",
                "researcher",
                "reviewer"
            ]
        );
        assert_eq!(
            set.stacks.keys().map(String::as_str).collect::<Vec<_>>(),
            ["nix", "rust-cargo"]
        );
        for (name, _) in ROLES {
            assert!(set.roles.contains_key(name));
        }
    }

    #[test]
    fn rust_implementer_renders_to_an_equivalent_of_the_host_card() {
        let set = curated_set();
        let rendered = set
            .render("rust-implementer")
            .unwrap()
            .with_repos(vec!["github.com/example/repo".into()]);
        let identity = rendered.to_identity("curated:rust-implementer").unwrap();

        let fixture = AgentIdentity::load(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/identities/global/rust-implementer.toml"
        ))
        .unwrap();
        assert_eq!(identity.identity.dev_loop, fixture.identity.dev_loop);
        assert_eq!(identity.scope.max_effect, EffectClass::Workspace);
        for tool in [
            "read_file",
            "write_file",
            "replace",
            "grep",
            "list_directory",
            "finish",
            "cargo_check",
            "cargo_test",
        ] {
            assert!(identity.allows_tool(tool), "{tool}");
        }
        for tool in ["run_command", "git_push_branch", "github_pr_open"] {
            assert!(!identity.allows_tool(tool), "{tool}");
        }
        assert_eq!(identity.limits.max_iterations, 200);
        assert_eq!(identity.limits.max_wall_clock_secs, 43200);
        assert_eq!(identity.limits.max_concurrent, 4);
        assert_eq!(identity.scope.repos, ["github.com/example/repo"]);
    }

    #[test]
    fn reviewer_and_researcher_have_no_exec_verbs_and_no_effect() {
        let set = curated_set();
        for role in ["reviewer", "researcher"] {
            let entry = &set.roles[role];
            assert!(entry.value.verbs.is_empty());
            assert_eq!(entry.value.max_effect, EffectClass::None);
            assert!(entry.value.write_globs.is_empty());
            assert!(!entry.value.tools.iter().any(|t| t == "write_file"));
        }
    }

    #[test]
    fn the_nix_stack_builds_and_checks_with_nix() {
        let set = curated_set();
        let nix = &set.stacks["nix"].value;
        assert_eq!(nix.verbs["build"].argv, ["nix", "build"]);
        assert_eq!(nix.verbs["check"].argv, ["nix", "flake", "check"]);
        assert_eq!(nix.detect, ["flake.nix"]);
    }

    #[test]
    fn the_same_role_renders_against_any_stack_that_defines_its_verbs() {
        let set = curated_set();
        let role = &set.roles["pr-shepherd"].value;
        for stack in set.stacks.values() {
            let comp = crate::plugins::types::Composition {
                name: "x".into(),
                role: role.name.clone(),
                stack: "s".into(),
                description: None,
                max_effect: None,
                limits: None,
            };
            let rendered =
                crate::plugins::render::render_composition(role, &stack.value, &comp).unwrap();
            assert_eq!(rendered.dev_loop, crate::identity::DevLoop::Middle);
        }
    }
}
