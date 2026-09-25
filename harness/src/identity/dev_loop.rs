//! The development-loop stage an identity operates in.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// Where in the software development lifecycle an identity acts.
///
/// The ordering follows the cost of a regression caught at each stage, so
/// `Inner < Middle < Outer`.
///
/// ```
/// use harness::identity::DevLoop;
///
/// assert_eq!("middle".parse::<DevLoop>(), Ok(DevLoop::Middle));
/// assert_eq!(DevLoop::Outer.to_string(), "outer");
/// assert!(DevLoop::Inner < DevLoop::Outer);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DevLoop {
    /// Before a pull request is opened: research, code generation, local checks.
    Inner,
    /// While a pull request is open: CI, review, sandbox QA.
    Middle,
    /// After merge: deployment, monitoring, incident response.
    Outer,
}

impl DevLoop {
    /// Every loop, from cheapest to most expensive.
    pub const ALL: [DevLoop; 3] = [DevLoop::Inner, DevLoop::Middle, DevLoop::Outer];

    /// Stable snake_case name, identical to the serde representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            DevLoop::Inner => "inner",
            DevLoop::Middle => "middle",
            DevLoop::Outer => "outer",
        }
    }
}

impl fmt::Display for DevLoop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error returned when parsing a string that names no [`DevLoop`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown loop `{0}`; expected one of inner, middle, outer")]
pub struct UnknownDevLoop(pub String);

impl FromStr for DevLoop {
    type Err = UnknownDevLoop;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        DevLoop::ALL
            .into_iter()
            .find(|dev_loop| dev_loop.as_str() == s)
            .ok_or_else(|| UnknownDevLoop(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn loops_are_ordered_by_regression_cost() {
        assert!(DevLoop::Inner < DevLoop::Middle);
        assert!(DevLoop::Middle < DevLoop::Outer);
        assert!(DevLoop::ALL.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn display_and_parse_use_snake_case() {
        assert_eq!(DevLoop::Inner.to_string(), "inner");
        assert_eq!(DevLoop::Middle.to_string(), "middle");
        assert_eq!(DevLoop::Outer.to_string(), "outer");
        assert_eq!("inner".parse::<DevLoop>(), Ok(DevLoop::Inner));
        assert_eq!("outer".parse::<DevLoop>(), Ok(DevLoop::Outer));
    }

    #[test]
    fn parse_rejects_unknown_and_wrong_case() {
        let err = "sideways".parse::<DevLoop>().unwrap_err();
        assert_eq!(err, UnknownDevLoop("sideways".to_string()));
        assert_eq!(
            err.to_string(),
            "unknown loop `sideways`; expected one of inner, middle, outer"
        );
        assert!("Inner".parse::<DevLoop>().is_err());
    }

    #[test]
    fn serde_uses_snake_case_strings() {
        assert_eq!(
            serde_json::to_string(&DevLoop::Middle).unwrap(),
            "\"middle\""
        );
        let parsed: DevLoop = serde_json::from_str("\"outer\"").unwrap();
        assert_eq!(parsed, DevLoop::Outer);
        assert!(serde_json::from_str::<DevLoop>("\"Outer\"").is_err());
    }

    proptest! {
        #[test]
        fn string_round_trip_is_identity(dev_loop in prop::sample::select(DevLoop::ALL.to_vec())) {
            prop_assert_eq!(dev_loop.as_str().parse::<DevLoop>(), Ok(dev_loop));
            prop_assert_eq!(dev_loop.to_string(), dev_loop.as_str());
        }
    }
}
