use super::{LeaseError, LeaseName};
use std::fmt;
use std::str::FromStr;

/// Minimal effect classification used to decide which leases an action
/// needs, ordered from least to most consequential.
///
/// This is a local stand-in; the shared effect model owned elsewhere maps
/// onto it by name via [`FromStr`].
///
/// ```
/// use harness::leases::Effect;
///
/// let effect: Effect = "Production".parse().unwrap();
/// assert_eq!(effect, Effect::Production);
/// assert!(Effect::Repository < Effect::Sandbox);
/// assert_eq!(effect.to_string(), "production");
/// assert!("cosmic".parse::<Effect>().is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Effect {
    /// Confined to the agent's own container or worktree.
    Local,
    /// Pushes to a shared repository.
    Repository,
    /// Deploys to a disposable sandbox environment.
    Sandbox,
    /// Rolls out to systems serving real traffic.
    Production,
}

impl Effect {
    /// Every effect, least consequential first.
    pub const ALL: [Effect; 4] = [
        Effect::Local,
        Effect::Repository,
        Effect::Sandbox,
        Effect::Production,
    ];

    /// Lower-case name.
    pub const fn name(self) -> &'static str {
        match self {
            Effect::Local => "local",
            Effect::Repository => "repository",
            Effect::Sandbox => "sandbox",
            Effect::Production => "production",
        }
    }
}

impl fmt::Display for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Effect {
    type Err = LeaseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Effect::ALL
            .into_iter()
            .find(|effect| effect.name().eq_ignore_ascii_case(s))
            .ok_or_else(|| LeaseError::UnknownEffect(s.to_string()))
    }
}

/// What an action is about to touch, from which its leases follow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LeaseContext<'a> {
    /// Repository in `owner/name` form.
    pub repo: &'a str,
    /// Branch pushed to, for repository effects.
    pub branch: Option<&'a str>,
    /// Pull request deployed, for sandbox effects.
    pub pr: Option<u64>,
    /// Environment rolled out to, for production effects.
    pub environment: Option<&'a str>,
    /// Path globs the action edits; a non-empty set adds a `paths` lease to
    /// every effect at or above `Repository`.
    pub paths: &'a [String],
}

/// Leases an action of `effect` must hold, in acquisition order.
///
/// `Repository` needs the branch lease, `Sandbox` the sandbox lease and
/// `Production` the deploy lease for its environment; `Local` needs none. A
/// missing field is an error rather than a silently shorter list.
///
/// ```
/// use harness::leases::{required_leases, Effect, LeaseContext, LeaseError, LeaseName};
///
/// let paths = vec!["src/**".to_string()];
/// let ctx = LeaseContext { repo: "example/repo", branch: Some("main"), pr: Some(7), environment: Some("prod"), paths: &paths };
///
/// assert!(required_leases(Effect::Local, &ctx).unwrap().is_empty());
/// assert_eq!(
///     required_leases(Effect::Repository, &ctx).unwrap(),
///     vec![LeaseName::branch("example/repo", "main"), LeaseName::paths("example/repo", &paths)]
/// );
/// assert_eq!(required_leases(Effect::Sandbox, &ctx).unwrap()[0], LeaseName::sandbox("example/repo", 7));
/// assert_eq!(required_leases(Effect::Production, &ctx).unwrap()[0], LeaseName::deploy("example/repo", "prod"));
///
/// let bare = LeaseContext { repo: "example/repo", ..LeaseContext::default() };
/// assert_eq!(
///     required_leases(Effect::Production, &bare),
///     Err(LeaseError::MissingContext { effect: Effect::Production, field: "environment" })
/// );
/// ```
pub fn required_leases(effect: Effect, ctx: &LeaseContext) -> Result<Vec<LeaseName>, LeaseError> {
    let missing = |field| LeaseError::MissingContext { effect, field };
    let primary = match effect {
        Effect::Local => return Ok(Vec::new()),
        Effect::Repository => LeaseName::branch(ctx.repo, ctx.branch.ok_or(missing("branch"))?),
        Effect::Sandbox => LeaseName::sandbox(ctx.repo, ctx.pr.ok_or(missing("pr"))?),
        Effect::Production => {
            LeaseName::deploy(ctx.repo, ctx.environment.ok_or(missing("environment"))?)
        }
    };
    let mut names = vec![primary];
    if !ctx.paths.is_empty() {
        names.push(LeaseName::paths(ctx.repo, ctx.paths));
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx<'a>(paths: &'a [String]) -> LeaseContext<'a> {
        LeaseContext {
            repo: "example/repo",
            branch: Some("feat/x"),
            pr: Some(12),
            environment: Some("staging"),
            paths,
        }
    }

    #[test]
    fn each_effect_maps_to_its_lease() {
        let none: Vec<String> = vec![];
        let c = ctx(&none);
        assert_eq!(required_leases(Effect::Local, &c).unwrap(), vec![]);
        assert_eq!(
            required_leases(Effect::Repository, &c).unwrap(),
            vec![LeaseName::branch("example/repo", "feat/x")]
        );
        assert_eq!(
            required_leases(Effect::Sandbox, &c).unwrap(),
            vec![LeaseName::sandbox("example/repo", 12)]
        );
        assert_eq!(
            required_leases(Effect::Production, &c).unwrap(),
            vec![LeaseName::deploy("example/repo", "staging")]
        );
    }

    #[test]
    fn paths_add_a_lease_above_local_in_acquisition_order() {
        let paths = vec!["b/**".to_string(), "a/**".to_string()];
        let c = ctx(&paths);
        assert!(required_leases(Effect::Local, &c).unwrap().is_empty());
        for effect in [Effect::Repository, Effect::Sandbox, Effect::Production] {
            let names = required_leases(effect, &c).unwrap();
            assert_eq!(names.len(), 2);
            assert_eq!(names[1], LeaseName::paths("example/repo", &paths));
            assert!(names[0] < names[1]);
        }
    }

    #[test]
    fn missing_context_is_an_error_naming_the_field() {
        let bare = LeaseContext {
            repo: "example/repo",
            ..LeaseContext::default()
        };
        for (effect, field) in [
            (Effect::Repository, "branch"),
            (Effect::Sandbox, "pr"),
            (Effect::Production, "environment"),
        ] {
            let err = required_leases(effect, &bare).unwrap_err();
            assert_eq!(err, LeaseError::MissingContext { effect, field });
            assert_eq!(
                err.to_string(),
                format!("{effect} effect needs `{field}` in its lease context")
            );
        }
        assert!(required_leases(Effect::Local, &bare).unwrap().is_empty());
    }

    #[test]
    fn effect_names_round_trip_and_order() {
        for effect in Effect::ALL {
            assert_eq!(effect.name().parse::<Effect>().unwrap(), effect);
            assert_eq!(
                effect.name().to_uppercase().parse::<Effect>().unwrap(),
                effect
            );
        }
        assert!(Effect::Local < Effect::Repository);
        assert!(Effect::Sandbox < Effect::Production);
        let err = "orbit".parse::<Effect>().unwrap_err();
        assert_eq!(err, LeaseError::UnknownEffect("orbit".to_string()));
        assert_eq!(
            err.to_string(),
            "unknown effect `orbit` (expected one of: local, repository, sandbox, production)"
        );
    }
}
