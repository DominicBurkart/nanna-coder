//! Effect taxonomy: every tool call is classified by the blast radius of
//! the side effects it can produce.
//!
//! The classes form a total order so that policy layers (RBAC, auditing,
//! scheduling, coordination locks) can express permissions as a single
//! ceiling: a tool is allowed when its declared class is at most the ceiling.
//!
//! A tool that can reach several classes (a shell runner, for example)
//! declares the *maximum* class it can reach and is treated as that class.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// Blast radius of a tool call, ordered from harmless to catastrophic.
///
/// The derived [`Ord`] follows declaration order, so comparisons express
/// containment: everything a `Workspace` tool can touch is also reachable
/// by a `Repository` tool, and so on up to `Production`.
///
/// ```
/// use harness::effects::EffectClass;
///
/// assert!(EffectClass::None < EffectClass::Workspace);
/// assert!(EffectClass::Workspace < EffectClass::Repository);
/// assert!(EffectClass::Repository < EffectClass::Ci);
/// assert!(EffectClass::Ci < EffectClass::Sandbox);
/// assert!(EffectClass::Sandbox < EffectClass::Production);
///
/// let ceiling = EffectClass::Workspace;
/// assert!(EffectClass::None <= ceiling);
/// assert!(EffectClass::Repository > ceiling);
/// assert_eq!(EffectClass::ALL.iter().max(), Some(&EffectClass::Production));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectClass {
    /// Pure read: no observable side effect anywhere.
    None,
    /// Writes confined to the task worktree or its dev container.
    Workspace,
    /// Writes to the shared repository host: push, branch, PR or issue writes.
    Repository,
    /// Triggers CI or other expensive shared test jobs.
    Ci,
    /// Deploys to a non-production environment.
    Sandbox,
    /// Touches live traffic or production configuration.
    Production,
}

impl EffectClass {
    /// Every class, in ascending blast-radius order.
    ///
    /// ```
    /// use harness::effects::EffectClass;
    ///
    /// assert!(EffectClass::ALL.windows(2).all(|pair| pair[0] < pair[1]));
    /// ```
    pub const ALL: [EffectClass; 6] = [
        EffectClass::None,
        EffectClass::Workspace,
        EffectClass::Repository,
        EffectClass::Ci,
        EffectClass::Sandbox,
        EffectClass::Production,
    ];

    /// Stable snake_case name, identical to the serde representation.
    ///
    /// ```
    /// use harness::effects::EffectClass;
    ///
    /// assert_eq!(EffectClass::Ci.as_str(), "ci");
    /// assert_eq!(EffectClass::Ci.as_str().parse::<EffectClass>(), Ok(EffectClass::Ci));
    /// ```
    pub const fn as_str(self) -> &'static str {
        match self {
            EffectClass::None => "none",
            EffectClass::Workspace => "workspace",
            EffectClass::Repository => "repository",
            EffectClass::Ci => "ci",
            EffectClass::Sandbox => "sandbox",
            EffectClass::Production => "production",
        }
    }

    /// Whether a call of this class can leave a trace anywhere.
    ///
    /// ```
    /// use harness::effects::EffectClass;
    ///
    /// assert!(!EffectClass::None.is_effectful());
    /// assert!(EffectClass::Workspace.is_effectful());
    /// ```
    pub const fn is_effectful(self) -> bool {
        !matches!(self, EffectClass::None)
    }
}

impl fmt::Display for EffectClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing a string that names no [`EffectClass`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown effect class: {0}")]
pub struct UnknownEffectClass(pub String);

impl FromStr for EffectClass {
    type Err = UnknownEffectClass;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EffectClass::ALL
            .into_iter()
            .find(|class| class.as_str() == s)
            .ok_or_else(|| UnknownEffectClass(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn classes_are_ordered_by_blast_radius() {
        assert!(EffectClass::None < EffectClass::Workspace);
        assert!(EffectClass::Workspace < EffectClass::Repository);
        assert!(EffectClass::Repository < EffectClass::Ci);
        assert!(EffectClass::Ci < EffectClass::Sandbox);
        assert!(EffectClass::Sandbox < EffectClass::Production);
    }

    #[test]
    fn all_lists_every_class_in_ascending_order() {
        assert_eq!(EffectClass::ALL.len(), 6);
        assert!(EffectClass::ALL.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(EffectClass::ALL.first(), Some(&EffectClass::None));
        assert_eq!(EffectClass::ALL.last(), Some(&EffectClass::Production));
    }

    #[test]
    fn display_uses_snake_case_names() {
        assert_eq!(EffectClass::None.to_string(), "none");
        assert_eq!(EffectClass::Workspace.to_string(), "workspace");
        assert_eq!(EffectClass::Repository.to_string(), "repository");
        assert_eq!(EffectClass::Ci.to_string(), "ci");
        assert_eq!(EffectClass::Sandbox.to_string(), "sandbox");
        assert_eq!(EffectClass::Production.to_string(), "production");
    }

    #[test]
    fn parse_rejects_unknown_names() {
        let err = "deploy_everywhere".parse::<EffectClass>().unwrap_err();
        assert_eq!(err, UnknownEffectClass("deploy_everywhere".to_string()));
        assert_eq!(err.to_string(), "unknown effect class: deploy_everywhere");
    }

    #[test]
    fn parse_is_case_sensitive() {
        assert!("Workspace".parse::<EffectClass>().is_err());
        assert_eq!(
            "workspace".parse::<EffectClass>(),
            Ok(EffectClass::Workspace)
        );
    }

    #[test]
    fn serde_uses_snake_case_strings() {
        let json = serde_json::to_string(&EffectClass::Ci).unwrap();
        assert_eq!(json, "\"ci\"");
        let parsed: EffectClass = serde_json::from_str("\"production\"").unwrap();
        assert_eq!(parsed, EffectClass::Production);
        assert!(serde_json::from_str::<EffectClass>("\"Ci\"").is_err());
    }

    #[test]
    fn only_none_is_effect_free() {
        assert!(!EffectClass::None.is_effectful());
        for class in EffectClass::ALL.into_iter().skip(1) {
            assert!(class.is_effectful(), "{class} should be effectful");
        }
    }

    fn any_class() -> impl Strategy<Value = EffectClass> {
        prop::sample::select(EffectClass::ALL.to_vec())
    }

    proptest! {
        #[test]
        fn ordering_matches_position_in_all(a in any_class(), b in any_class()) {
            let pos = |c: EffectClass| EffectClass::ALL.iter().position(|x| *x == c).unwrap();
            prop_assert_eq!(a.cmp(&b), pos(a).cmp(&pos(b)));
        }

        #[test]
        fn string_round_trip_is_identity(class in any_class()) {
            prop_assert_eq!(class.as_str().parse::<EffectClass>(), Ok(class));
            prop_assert_eq!(class.to_string(), class.as_str());
        }

        #[test]
        fn serde_round_trip_is_identity(class in any_class()) {
            let json = serde_json::to_string(&class).unwrap();
            prop_assert_eq!(&json, &format!("\"{}\"", class.as_str()));
            let back: EffectClass = serde_json::from_str(&json).unwrap();
            prop_assert_eq!(back, class);
        }
    }
}
