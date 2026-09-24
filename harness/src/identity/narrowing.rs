//! The narrowing-only rule for repo-local identity overrides.
//!
//! A repo-local identity that shares a name with a global one replaces it in
//! the catalog, but only if it grants no more than the global base. "No more"
//! is defined field by field, without interpreting globs:
//!
//! - `scope.max_effect` must be at or below the base ceiling;
//! - `scope.repos`, `scope.paths` and `scope.tools` must each be a subset of
//!   the base list, compared as exact strings (so `api/**` in the override
//!   requires `api/**` in the base, not merely a base glob that covers it);
//! - `scope.read_paths` is unrestricted (`None`) in the base, or the override
//!   must also restrict reads to a subset of the base list;
//! - every `[limits]` value must be at or below the base value.
//!
//! `identity.description`, `identity.loop`, `identity.model` and
//! `identity.system_prompt` may differ freely: they change what the agent is
//! told, not what it can reach.

use super::{AgentIdentity, IdentityError};

impl AgentIdentity {
    /// Check that `self` grants no more than `base` (see the [module docs](self)).
    ///
    /// ```
    /// use harness::identity::AgentIdentity;
    ///
    /// let toml = r#"
    /// [identity]
    /// name = "pr-shepherd"
    /// description = "Keeps a pull request green until it is mergeable."
    /// loop = "middle"
    /// model = "gemma4:e4b"
    /// system_prompt = { inline = "Shepherd the PR." }
    ///
    /// [scope]
    /// repos = ["github.com/example/repo", "github.com/example/other"]
    /// paths = ["src/**", "tests/**"]
    /// max_effect = "ci"
    /// tools = ["read_file", "write_file", "git_*", "github_pr_status"]
    ///
    /// [limits]
    /// max_iterations = 100
    /// max_wall_clock_secs = 1800
    /// max_concurrent = 2
    /// "#;
    /// let base = AgentIdentity::from_toml_str(toml, "global/pr-shepherd.toml").unwrap();
    ///
    /// let narrower = toml
    ///     .replace("max_effect = \"ci\"", "max_effect = \"repository\"")
    ///     .replace(", \"github_pr_status\"", "")
    ///     .replace("max_concurrent = 2", "max_concurrent = 1");
    /// let local = AgentIdentity::from_toml_str(&narrower, ".nanna/agents/pr-shepherd.toml").unwrap();
    /// assert!(local.narrows(&base).is_ok());
    ///
    /// let wider = toml.replace("max_effect = \"ci\"", "max_effect = \"sandbox\"");
    /// let local = AgentIdentity::from_toml_str(&wider, ".nanna/agents/pr-shepherd.toml").unwrap();
    /// let err = local.narrows(&base).unwrap_err();
    /// assert!(err.to_string().contains("scope.max_effect"), "{err}");
    /// ```
    pub fn narrows(&self, base: &AgentIdentity) -> Result<(), IdentityError> {
        let name = self.identity.name.as_str();
        if self.scope.max_effect > base.scope.max_effect {
            let reason = format!(
                "{} exceeds {}",
                self.scope.max_effect, base.scope.max_effect
            );
            return Err(widens(name, "scope.max_effect", reason));
        }
        subset(name, "scope.repos", &self.scope.repos, &base.scope.repos)?;
        subset(name, "scope.paths", &self.scope.paths, &base.scope.paths)?;
        let own_tools: Vec<String> = self.scope.tools.iter().map(ToString::to_string).collect();
        let base_tools: Vec<String> = base.scope.tools.iter().map(ToString::to_string).collect();
        subset(name, "scope.tools", &own_tools, &base_tools)?;
        match (&self.scope.read_paths, &base.scope.read_paths) {
            (_, None) => {}
            (None, Some(_)) => {
                let reason = "reads are unrestricted but the base restricts them".to_string();
                return Err(widens(name, "scope.read_paths", reason));
            }
            (Some(own), Some(base)) => subset(name, "scope.read_paths", own, base)?,
        }
        let (own, base_limits) = (self.limits, base.limits);
        at_most(
            name,
            "limits.max_iterations",
            own.max_iterations as u64,
            base_limits.max_iterations as u64,
        )?;
        at_most(
            name,
            "limits.max_wall_clock_secs",
            own.max_wall_clock_secs,
            base_limits.max_wall_clock_secs,
        )?;
        at_most(
            name,
            "limits.max_concurrent",
            own.max_concurrent as u64,
            base_limits.max_concurrent as u64,
        )?;
        Ok(())
    }
}

fn widens(name: &str, field: &str, reason: String) -> IdentityError {
    IdentityError::WidensScope {
        name: name.to_string(),
        field: field.to_string(),
        reason,
    }
}

fn subset(name: &str, field: &str, own: &[String], base: &[String]) -> Result<(), IdentityError> {
    match own.iter().find(|item| !base.contains(item)) {
        Some(extra) => Err(widens(
            name,
            field,
            format!("`{extra}` is not in the base {field}"),
        )),
        None => Ok(()),
    }
}

fn at_most(name: &str, field: &str, own: u64, base: u64) -> Result<(), IdentityError> {
    if own > base {
        return Err(widens(
            name,
            field,
            format!("{own} exceeds the base {field} of {base}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::schema::tests::{
        any_identity, example, path_glob_strategy, repo_strategy, tool_pattern_strategy,
    };
    use super::super::ToolPattern;
    use super::*;
    use crate::effects::EffectClass;
    use proptest::prelude::*;

    fn widened_field(result: Result<(), IdentityError>) -> String {
        match result {
            Err(IdentityError::WidensScope {
                name,
                field,
                reason,
            }) => {
                assert_eq!(name, "rust-implementer");
                assert!(!reason.is_empty());
                field
            }
            other => panic!("expected WidensScope, got {other:?}"),
        }
    }

    #[test]
    fn an_identity_narrows_itself() {
        let base = example();
        assert!(base.narrows(&base).is_ok());
    }

    #[test]
    fn prompt_loop_model_and_description_may_differ() {
        let base = example();
        let mut local = base.clone();
        local.identity.description = "Repo-specific implementer.".to_string();
        local.identity.dev_loop = super::super::DevLoop::Middle;
        local.identity.model = "other:model".to_string();
        local.identity.system_prompt =
            super::super::SystemPrompt::Inline("Be careful.".to_string());
        assert!(local.narrows(&base).is_ok());
    }

    #[test]
    fn raising_max_effect_widens() {
        let base = example();
        let mut local = base.clone();
        local.scope.max_effect = EffectClass::Ci;
        let err = local.narrows(&base).unwrap_err();
        assert_eq!(err.to_string(), "repo-local identity `rust-implementer` widens `scope.max_effect` of its global base: ci exceeds repository");
        local.scope.max_effect = EffectClass::Workspace;
        assert!(local.narrows(&base).is_ok());
    }

    #[test]
    fn adding_a_repo_path_or_tool_widens() {
        let base = example();
        let mut local = base.clone();
        local
            .scope
            .repos
            .push("github.com/example/other".to_string());
        assert_eq!(widened_field(local.narrows(&base)), "scope.repos");

        let mut local = base.clone();
        local.scope.paths = vec!["api/**".to_string(), "docs/**".to_string()];
        assert_eq!(widened_field(local.narrows(&base)), "scope.paths");

        let mut local = base.clone();
        local
            .scope
            .tools
            .push(ToolPattern::new("run_command").unwrap());
        assert_eq!(widened_field(local.narrows(&base)), "scope.tools");
    }

    #[test]
    fn subset_is_by_exact_string_not_by_glob_coverage() {
        let base = example();
        let mut local = base.clone();
        local.scope.paths = vec!["api/handlers/**".to_string()];
        assert_eq!(widened_field(local.narrows(&base)), "scope.paths");
        let mut local = base.clone();
        local.scope.tools = vec![ToolPattern::new("cargo_check").unwrap()];
        let err = local.narrows(&base).unwrap_err();
        assert!(
            err.to_string()
                .contains("`cargo_check` is not in the base scope.tools"),
            "{err}"
        );
    }

    #[test]
    fn dropping_entries_narrows() {
        let base = example();
        let mut local = base.clone();
        local.scope.repos.clear();
        local.scope.paths = vec!["shared/**".to_string()];
        local.scope.tools = vec![ToolPattern::new("read_file").unwrap()];
        assert!(local.narrows(&base).is_ok());
    }

    #[test]
    fn read_paths_follow_the_base_restriction() {
        let base = example();
        let mut local = base.clone();
        local.scope.read_paths = Some(vec!["docs/**".to_string()]);
        assert!(
            local.narrows(&base).is_ok(),
            "restricting unrestricted reads narrows"
        );

        let mut restricted = base.clone();
        restricted.scope.read_paths = Some(vec!["docs/**".to_string(), "api/**".to_string()]);
        let mut local = restricted.clone();
        local.scope.read_paths = None;
        let err = local.narrows(&restricted).unwrap_err();
        assert!(err.to_string().contains("scope.read_paths"), "{err}");
        local.scope.read_paths = Some(vec!["docs/**".to_string()]);
        assert!(local.narrows(&restricted).is_ok());
        local.scope.read_paths = Some(vec!["src/**".to_string()]);
        assert_eq!(
            widened_field(local.narrows(&restricted)),
            "scope.read_paths"
        );
    }

    #[test]
    fn raising_any_limit_widens() {
        let base = example();
        let mut local = base.clone();
        local.limits.max_iterations += 1;
        assert_eq!(widened_field(local.narrows(&base)), "limits.max_iterations");
        let mut local = base.clone();
        local.limits.max_wall_clock_secs += 1;
        assert_eq!(
            widened_field(local.narrows(&base)),
            "limits.max_wall_clock_secs"
        );
        let mut local = base.clone();
        local.limits.max_concurrent += 1;
        let err = local.narrows(&base).unwrap_err();
        assert_eq!(err.to_string(), "repo-local identity `rust-implementer` widens `limits.max_concurrent` of its global base: 5 exceeds the base limits.max_concurrent of 4");
        local.limits.max_concurrent = 1;
        assert!(local.narrows(&base).is_ok());
    }

    fn sublist(items: &[String]) -> impl Strategy<Value = Vec<String>> {
        let items = items.to_vec();
        prop::collection::vec(any::<bool>(), items.len()).prop_map(move |keep| {
            items
                .iter()
                .zip(keep)
                .filter(|(_, k)| *k)
                .map(|(i, _)| i.clone())
                .collect()
        })
    }

    fn narrowed_from(base: AgentIdentity) -> impl Strategy<Value = (AgentIdentity, AgentIdentity)> {
        let effects: Vec<EffectClass> = EffectClass::ALL
            .iter()
            .copied()
            .filter(|c| *c <= base.scope.max_effect)
            .collect();
        let tools: Vec<String> = base.scope.tools.iter().map(ToString::to_string).collect();
        let read_paths = match &base.scope.read_paths {
            None => prop::option::of(prop::collection::vec(path_glob_strategy(), 0..3)).boxed(),
            Some(list) => sublist(list).prop_map(Some).boxed(),
        };
        let scope = (
            sublist(&base.scope.repos),
            sublist(&base.scope.paths),
            sublist(&tools),
            prop::sample::select(effects),
            read_paths,
        );
        let limits = (
            1..=base.limits.max_iterations,
            1..=base.limits.max_wall_clock_secs,
            1..=base.limits.max_concurrent,
        );
        (scope, limits).prop_map(
            move |(
                (repos, paths, tools, max_effect, read_paths),
                (max_iterations, max_wall_clock_secs, max_concurrent),
            )| {
                let mut local = base.clone();
                local.scope.repos = repos;
                local.scope.paths = paths;
                local.scope.tools = tools.iter().map(|t| ToolPattern::new(t).unwrap()).collect();
                local.scope.max_effect = max_effect;
                local.scope.read_paths = read_paths;
                local.limits.max_iterations = max_iterations;
                local.limits.max_wall_clock_secs = max_wall_clock_secs;
                local.limits.max_concurrent = max_concurrent;
                (base.clone(), local)
            },
        )
    }

    fn base_and_override() -> impl Strategy<Value = (AgentIdentity, AgentIdentity)> {
        any_identity().prop_flat_map(narrowed_from)
    }

    fn base_middle_bottom() -> impl Strategy<Value = (AgentIdentity, AgentIdentity, AgentIdentity)>
    {
        base_and_override().prop_flat_map(|(base, middle)| {
            narrowed_from(middle).prop_map(move |(middle, bottom)| (base.clone(), middle, bottom))
        })
    }

    proptest! {
        #[test]
        fn narrowing_is_reflexive(identity in any_identity()) {
            prop_assert!(identity.narrows(&identity).is_ok());
        }

        #[test]
        fn every_derived_override_narrows_its_base((base, local) in base_and_override()) {
            prop_assert!(local.narrows(&base).is_ok(), "{:?}", local.narrows(&base));
        }

        #[test]
        fn a_narrowed_override_never_allows_more((base, local) in base_and_override(), tool in "[a-z][a-z0-9_]{0,12}", class in prop::sample::select(EffectClass::ALL.to_vec())) {
            prop_assert!(!local.allows_tool(&tool) || base.allows_tool(&tool));
            prop_assert!(!local.allows_effect(class) || base.allows_effect(class));
        }

        #[test]
        fn widening_one_field_is_rejected_naming_it((base, local) in base_and_override(), choice in 0u8..7, repo in repo_strategy(), path in path_glob_strategy(), tool in tool_pattern_strategy()) {
            let mut wider = local.clone();
            let expected = match choice {
                0 => {
                    prop_assume!(base.scope.max_effect < EffectClass::Production);
                    wider.scope.max_effect = EffectClass::Production;
                    "scope.max_effect"
                }
                1 => {
                    prop_assume!(!base.scope.repos.contains(&repo));
                    wider.scope.repos.push(repo);
                    "scope.repos"
                }
                2 => {
                    prop_assume!(!base.scope.paths.contains(&path));
                    wider.scope.paths.push(path);
                    "scope.paths"
                }
                3 => {
                    prop_assume!(!base.scope.tools.iter().any(|t| t.as_str() == tool));
                    wider.scope.tools.push(ToolPattern::new(&tool).unwrap());
                    "scope.tools"
                }
                4 => {
                    wider.limits.max_iterations = base.limits.max_iterations + 1;
                    "limits.max_iterations"
                }
                5 => {
                    wider.limits.max_wall_clock_secs = base.limits.max_wall_clock_secs + 1;
                    "limits.max_wall_clock_secs"
                }
                _ => {
                    wider.limits.max_concurrent = base.limits.max_concurrent + 1;
                    "limits.max_concurrent"
                }
            };
            match wider.narrows(&base) {
                Err(IdentityError::WidensScope { field, .. }) => prop_assert_eq!(field, expected),
                other => prop_assert!(false, "expected WidensScope on {expected}, got {other:?}"),
            }
        }

        #[test]
        fn narrowing_is_transitive((base, middle, bottom) in base_middle_bottom()) {
            prop_assert!(bottom.narrows(&middle).is_ok());
            prop_assert!(middle.narrows(&base).is_ok());
            prop_assert!(bottom.narrows(&base).is_ok());
        }
    }
}
