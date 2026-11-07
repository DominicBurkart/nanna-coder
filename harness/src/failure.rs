use serde::{Deserialize, Serialize};

macro_rules! failure_kinds {
    ($(#[$meta:meta])* $($variant:ident => $name:literal),+ $(,)?) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub enum FailureKind {
            $(
                #[serde(rename = $name)]
                $variant,
            )+
        }

        impl FailureKind {
            pub const ALL: &'static [FailureKind] = &[$(FailureKind::$variant),+];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(FailureKind::$variant => $name,)+
                }
            }
        }
    };
}

failure_kinds! {
    ///
    /// ```
    /// use harness::failure::FailureKind;
    ///
    /// assert_eq!(FailureKind::from_error_type("MaxIterationsExceeded"), FailureKind::MaxIterations);
    /// assert_eq!(FailureKind::from_error_type("ProtectedPathViolation").as_str(), "protected_path_violation");
    /// assert_eq!(FailureKind::from_error_type("SomethingNew"), FailureKind::Unclassified);
    /// assert!(FailureKind::ALL.contains(&FailureKind::SpawnRefused));
    /// ```
    MaxIterations => "max_iterations",
    StateError => "state_error",
    TaskCheckFailed => "task_check_failed",
    ProtectedPathViolation => "protected_path_violation",
    ScopeError => "scope_error",
    IdentityUnavailable => "identity_unavailable",
    ModelUnavailable => "model_unavailable",
    ContainerSetupFailed => "container_setup_failed",
    WorkspaceSetupFailed => "workspace_setup_failed",
    ImpactError => "impact_error",
    QueueError => "queue_error",
    RestoreRefused => "restore_refused",
    SpawnRefused => "spawn_refused",
    InvalidRequest => "invalid_request",
    Cancelled => "cancelled",
    Unclassified => "unclassified",
}

impl FailureKind {
    pub fn from_error_type(error_type: &str) -> FailureKind {
        match error_type {
            "MaxIterationsExceeded" => FailureKind::MaxIterations,
            "StateError" => FailureKind::StateError,
            "TaskCheckFailed" => FailureKind::TaskCheckFailed,
            "ProtectedPathViolation" => FailureKind::ProtectedPathViolation,
            "ScopeError" => FailureKind::ScopeError,
            "IdentityUnavailable" => FailureKind::IdentityUnavailable,
            "NoProvider" => FailureKind::ModelUnavailable,
            "ContainerSetupFailed" | "SidecarSetupFailed" => FailureKind::ContainerSetupFailed,
            "WorkspaceCreationFailed" => FailureKind::WorkspaceSetupFailed,
            "ImpactError" => FailureKind::ImpactError,
            "QueuePersistFailed" => FailureKind::QueueError,
            "RestoreNoIdentity"
            | "RestoreOverridesInvalid"
            | "RestoreRefused"
            | "RestoreAuditFailed"
            | "RestoreAuditDeferred" => FailureKind::RestoreRefused,
            _ => FailureKind::Unclassified,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashSet;

    const KNOWN_LABELS: &[(&str, FailureKind)] = &[
        ("MaxIterationsExceeded", FailureKind::MaxIterations),
        ("StateError", FailureKind::StateError),
        ("TaskCheckFailed", FailureKind::TaskCheckFailed),
        (
            "ProtectedPathViolation",
            FailureKind::ProtectedPathViolation,
        ),
        ("ScopeError", FailureKind::ScopeError),
        ("IdentityUnavailable", FailureKind::IdentityUnavailable),
        ("NoProvider", FailureKind::ModelUnavailable),
        ("ContainerSetupFailed", FailureKind::ContainerSetupFailed),
        ("SidecarSetupFailed", FailureKind::ContainerSetupFailed),
        ("WorkspaceCreationFailed", FailureKind::WorkspaceSetupFailed),
        ("ImpactError", FailureKind::ImpactError),
        ("QueuePersistFailed", FailureKind::QueueError),
        ("RestoreNoIdentity", FailureKind::RestoreRefused),
        ("RestoreOverridesInvalid", FailureKind::RestoreRefused),
        ("RestoreRefused", FailureKind::RestoreRefused),
        ("RestoreAuditFailed", FailureKind::RestoreRefused),
        ("RestoreAuditDeferred", FailureKind::RestoreRefused),
    ];

    #[test]
    fn every_known_label_maps_to_its_kind() {
        for (label, kind) in KNOWN_LABELS {
            assert_eq!(FailureKind::from_error_type(label), *kind, "{label}");
        }
    }

    #[test]
    fn unknown_labels_are_unclassified() {
        for label in [
            "",
            "maxiterationsexceeded",
            "Scripted",
            "E",
            "MaxIterationsExceeded ",
        ] {
            assert_eq!(
                FailureKind::from_error_type(label),
                FailureKind::Unclassified,
                "{label:?}"
            );
        }
    }

    #[test]
    fn names_are_unique_snake_case() {
        let names: HashSet<&str> = FailureKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(names.len(), FailureKind::ALL.len());
        for name in names {
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{name}"
            );
            assert!(!name.starts_with('_') && !name.ends_with('_'), "{name}");
        }
    }

    #[test]
    fn serde_name_matches_as_str() {
        for kind in FailureKind::ALL {
            let json = serde_json::to_string(kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            assert_eq!(serde_json::from_str::<FailureKind>(&json).unwrap(), *kind);
        }
    }

    #[test]
    fn only_non_label_kinds_are_unreachable_from_labels() {
        let reachable: HashSet<FailureKind> = KNOWN_LABELS.iter().map(|(_, kind)| *kind).collect();
        let unreachable: HashSet<FailureKind> = FailureKind::ALL
            .iter()
            .copied()
            .filter(|kind| !reachable.contains(kind))
            .collect();
        let expected: HashSet<FailureKind> = [
            FailureKind::SpawnRefused,
            FailureKind::InvalidRequest,
            FailureKind::Cancelled,
            FailureKind::Unclassified,
        ]
        .into_iter()
        .collect();
        assert_eq!(unreachable, expected);
    }

    #[test]
    fn every_error_type_literal_in_task_rs_is_classified() {
        let source = include_str!("task.rs");
        let production = production_part(source);
        let mut seen = HashSet::new();
        for literal in quoted_words(&production) {
            let pascal = literal
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
                && literal.chars().filter(|c| c.is_ascii_uppercase()).count() >= 2;
            if !pascal {
                continue;
            }
            if KNOWN_LABELS.iter().any(|(label, _)| *label == literal) {
                seen.insert(literal.to_string());
            } else {
                assert!(
                    PASCAL_LITERALS_THAT_ARE_NOT_ERROR_TYPES.contains(&literal),
                    "task.rs has the string literal {literal:?}; if it is an error_type, add it to \
                     FailureKind::from_error_type and KNOWN_LABELS, otherwise to \
                     PASCAL_LITERALS_THAT_ARE_NOT_ERROR_TYPES"
                );
            }
        }
        for (label, _) in KNOWN_LABELS {
            assert!(
                seen.contains(*label),
                "{label} no longer appears in task.rs"
            );
        }
    }

    fn production_part(source: &str) -> String {
        source
            .replace("\r\n", "\n")
            .split("\n#[cfg(test)]\n")
            .next()
            .unwrap()
            .to_string()
    }

    #[test]
    fn the_production_region_is_found_with_unix_and_windows_line_endings() {
        assert_eq!(
            production_part("fn a() {}\n#[cfg(test)]\nmod tests {}\n"),
            "fn a() {}"
        );
        assert_eq!(
            production_part("fn a() {}\r\n#[cfg(test)]\r\nmod tests {}\r\n"),
            "fn a() {}"
        );
        assert_eq!(production_part("fn a() {}\n"), "fn a() {}\n");
    }

    fn quoted_words(source: &str) -> Vec<&str> {
        let bytes = source.as_bytes();
        let mut words = Vec::new();
        let mut start = 0;
        while let Some(offset) = source[start..].find('"') {
            let open = start + offset + 1;
            let len = source[open..]
                .bytes()
                .take_while(|b| b.is_ascii_alphanumeric())
                .count();
            if len > 0 && bytes.get(open + len) == Some(&b'"') {
                words.push(&source[open..open + len]);
            }
            start = open;
        }
        words
    }

    const PASCAL_LITERALS_THAT_ARE_NOT_ERROR_TYPES: &[&str] = &[];

    proptest! {
        #[test]
        fn classification_is_total_and_deterministic(label in ".{0,40}") {
            let first = FailureKind::from_error_type(&label);
            prop_assert_eq!(first, FailureKind::from_error_type(&label));
            if !KNOWN_LABELS.iter().any(|(known, _)| *known == label) {
                prop_assert_eq!(first, FailureKind::Unclassified);
            }
        }
    }
}
