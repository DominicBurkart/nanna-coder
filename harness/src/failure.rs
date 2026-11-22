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
    #[doc = r#"
```
use harness::failure::FailureKind;

assert_eq!(FailureKind::from_error_type("MaxIterationsExceeded"), FailureKind::MaxIterations);
assert_eq!(FailureKind::from_error_type("ProtectedPathViolation").as_str(), "protected_path_violation");
assert_eq!(FailureKind::from_error_type("SomethingNew"), FailureKind::Unclassified);
assert!(FailureKind::ALL.contains(&FailureKind::SpawnRefused));
```"#]
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
            "LineageInconsistent" => FailureKind::SpawnRefused,
            "IdentityUnavailable" => FailureKind::IdentityUnavailable,
            "NoProvider" => FailureKind::ModelUnavailable,
            "ContainerSetupFailed" | "SidecarSetupFailed" => FailureKind::ContainerSetupFailed,
            "WorkspaceCreationFailed" => FailureKind::WorkspaceSetupFailed,
            "ImpactError" => FailureKind::ImpactError,
            "QueuePersistFailed" | "SessionRegistryFailed" | "RestoreSessionRegistryFailed" => {
                FailureKind::QueueError
            }
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
        ("LineageInconsistent", FailureKind::SpawnRefused),
        ("IdentityUnavailable", FailureKind::IdentityUnavailable),
        ("NoProvider", FailureKind::ModelUnavailable),
        ("ContainerSetupFailed", FailureKind::ContainerSetupFailed),
        ("SidecarSetupFailed", FailureKind::ContainerSetupFailed),
        ("WorkspaceCreationFailed", FailureKind::WorkspaceSetupFailed),
        ("ImpactError", FailureKind::ImpactError),
        ("QueuePersistFailed", FailureKind::QueueError),
        ("SessionRegistryFailed", FailureKind::QueueError),
        ("RestoreSessionRegistryFailed", FailureKind::QueueError),
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
            FailureKind::InvalidRequest,
            FailureKind::Cancelled,
            FailureKind::Unclassified,
        ]
        .into_iter()
        .collect();
        assert_eq!(unreachable, expected);
    }

    #[test]
    fn every_label_token_in_task_rs_is_classified() {
        let production = production_text(include_str!("task.rs")).unwrap();
        let tokens = label_tokens(&production);
        assert!(tokens.len() > 15, "{}", tokens.len());
        let mut seen = HashSet::new();
        for token in &tokens {
            if KNOWN_LABELS.iter().any(|(label, _)| label == token) {
                seen.insert(token.clone());
            } else {
                assert!(
                    NON_LABEL_TOKENS.contains(&token.as_str()),
                    "task.rs passes the token {token:?} where an error_type label goes; add it to \
                     FailureKind::from_error_type and KNOWN_LABELS, or if it is not a label to \
                     NON_LABEL_TOKENS"
                );
            }
        }
        let constants: HashSet<String> = crate_error_constants()
            .into_iter()
            .map(|(_, label)| label)
            .collect();
        for (label, _) in KNOWN_LABELS {
            assert!(
                seen.contains(*label) || constants.contains(*label),
                "{label} no longer appears in task.rs or in an ERROR_* constant"
            );
        }
    }

    const NON_LABEL_TOKENS: &[&str] = &[];

    const ITEM_STARTS: &[&str] = &[
        "fn ",
        "async fn ",
        "pub ",
        "impl",
        "struct ",
        "enum ",
        "trait ",
        "mod ",
        "use ",
        "const ",
        "static ",
        "type ",
        "unsafe ",
        "extern ",
        "macro_rules!",
    ];

    const TEST_ONLY_GATES: &[&str] = &["#[cfg(test)]", "#[cfg(kani)]"];

    fn production_text(source: &str) -> Result<String, String> {
        let text = source.replace("\r\n", "\n");
        let lines: Vec<&str> = text.split('\n').collect();
        for (index, line) in lines.iter().enumerate() {
            if line.starts_with("#[cfg(") && !TEST_ONLY_GATES.contains(line) {
                return Err(format!(
                    "line {}: an unindented cfg attribute other than #[cfg(test)] makes the \
                     production boundary ambiguous: {line}",
                    index + 1
                ));
            }
        }
        let Some(first) = lines.iter().position(|line| *line == "#[cfg(test)]") else {
            return Ok(text);
        };
        let mut gated = true;
        for (offset, line) in lines[first + 1..].iter().enumerate() {
            if TEST_ONLY_GATES.contains(line) {
                gated = true;
            } else if line.starts_with("#[") || line.starts_with("#![") {
                continue;
            } else if ITEM_STARTS.iter().any(|start| line.starts_with(start)) {
                if !gated {
                    return Err(format!(
                        "line {}: production item after a #[cfg(test)] item: {line}",
                        first + offset + 2
                    ));
                }
                gated = false;
            }
        }
        Ok(lines[..first].join("\n"))
    }

    fn skip_string(bytes: &[u8], open: usize) -> usize {
        let mut index = open + 1;
        while index < bytes.len() && bytes[index] != b'"' {
            index += if bytes[index] == b'\\' { 2 } else { 1 };
        }
        index + 1
    }

    fn bare_token(bytes: &[u8], open: usize, close: usize) -> Option<String> {
        let inner = &bytes[open + 1..close - 1];
        let bare = !inner.is_empty()
            && inner
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
        bare.then(|| String::from_utf8_lossy(inner).into_owned())
    }

    fn tokens_until_balanced(source: &str, start: usize, open: u8, close: u8) -> Vec<String> {
        let bytes = source.as_bytes();
        let mut tokens = Vec::new();
        let mut depth = 0usize;
        let mut index = start;
        while index < bytes.len() {
            match bytes[index] {
                b'"' => {
                    let end = skip_string(bytes, index);
                    tokens.extend(bare_token(bytes, index, end));
                    index = end;
                    continue;
                }
                byte if byte == open => depth += 1,
                byte if byte == close => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
            index += 1;
        }
        tokens
    }

    fn label_tokens(production: &str) -> Vec<String> {
        let bytes = production.as_bytes();
        let mut tokens = Vec::new();
        for marker in ["fail(", "park("] {
            for (at, _) in production.match_indices(marker) {
                tokens.extend(tokens_until_balanced(
                    production,
                    at + marker.len() - 1,
                    b'(',
                    b')',
                ));
            }
        }
        for (at, _) in production.match_indices("error_type:") {
            let mut index = at + "error_type:".len();
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if bytes.get(index) == Some(&b'"') {
                tokens.extend(bare_token(bytes, index, skip_string(bytes, index)));
            }
        }
        for (at, _) in production.match_indices("error_type(") {
            let line_start = production[..at]
                .rfind('\n')
                .map_or(0, |newline| newline + 1);
            if production[line_start..at].contains("fn ") {
                if let Some(brace) = production[at..].find('{') {
                    tokens.extend(tokens_until_balanced(production, at + brace, b'{', b'}'));
                }
            }
        }
        tokens
    }

    #[test]
    fn label_tokens_are_found_in_any_casing_in_every_label_position() {
        let fixture = concat!(
            "impl R {\n",
            "    fn error_type(&self) -> &'static str {\n",
            "        match self { A => \"Foo\", B => \"snake_case\", C => \"X1\" }\n",
            "    }\n",
            "    fn run(&self) {\n",
            "        self.fail(&id, format!(\"closing ) paren {x}\"), \"Bad_Label\").await;\n",
            "        self.park(\n",
            "            &queued,\n",
            "            \"a message with spaces\".to_string(),\n",
            "            \"lowercase\",\n",
            "        )\n",
            "        .await;\n",
            "        let d = D { error_type: \"X\".to_string(), other: \"Ignored\" };\n",
            "        let e = D { error_type: label, other: \"AlsoIgnored\" };\n",
            "        self.fail(&id, e, crate::verify::ERROR_THING).await;\n",
            "        let f = \"NotInAnyLabelPosition\";\n",
            "    }\n",
            "}\n",
        );
        let mut found = label_tokens(fixture);
        found.sort();
        assert_eq!(
            found,
            ["Bad_Label", "Foo", "X", "X1", "lowercase", "snake_case"]
        );
    }

    #[test]
    fn a_production_label_outside_the_known_set_is_what_the_task_scan_rejects() {
        let production = production_text(
            "fn go(r: &R) { r.fail(&id, e, \"Foo\"); r.fail(&id, e, \"Bad_Label\"); }\n#[cfg(test)]\nmod tests {}\n",
        )
        .unwrap();
        let unknown: Vec<String> = label_tokens(&production)
            .into_iter()
            .filter(|token| !KNOWN_LABELS.iter().any(|(label, _)| label == token))
            .collect();
        assert_eq!(unknown, ["Foo", "Bad_Label"]);
    }

    #[test]
    fn the_production_region_is_found_with_unix_and_windows_line_endings() {
        assert_eq!(
            production_text("fn a() {}\n#[cfg(test)]\nmod tests {}\n").unwrap(),
            "fn a() {}"
        );
        assert_eq!(
            production_text("fn a() {}\r\n#[cfg(test)]\r\nmod tests {}\r\n").unwrap(),
            "fn a() {}"
        );
        assert_eq!(production_text("fn a() {}\n").unwrap(), "fn a() {}\n");
    }

    #[test]
    fn the_production_boundary_fails_closed_when_it_is_ambiguous() {
        let after = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n\nfn production_again() {\n    r.fail(&id, e, \"Foo\");\n}\n";
        assert!(production_text(after)
            .unwrap_err()
            .contains("production item after"));
        let derived = "fn a() {}\n#[cfg(test)]\nmod tests {}\n#[derive(Debug)]\nstruct Late;\n";
        assert!(production_text(derived).is_err());
        let not_test = "fn a() {}\n#[cfg(not(test))]\nfn b() {}\n#[cfg(test)]\nmod tests {}\n";
        assert!(production_text(not_test).unwrap_err().contains("ambiguous"));
        let combined = "fn a() {}\n#[cfg(all(test, unix))]\nmod tests {}\n";
        assert!(production_text(combined).is_err());
        let two_modules = "fn a() {}\n#[cfg(test)]\nmod one {\n}\n\n#[cfg(test)]\nmod two {\n}\n";
        assert_eq!(production_text(two_modules).unwrap(), "fn a() {}");
        let indented = "fn a() {\n    #[cfg(not(test))]\n    x();\n}\n#[cfg(test)]\nmod tests {}\n";
        assert!(production_text(indented).unwrap().contains("x();"));
    }

    fn error_constants(source: &str) -> Vec<String> {
        source
            .lines()
            .filter_map(|line| {
                let rest = line.trim_start();
                let rest = rest
                    .strip_prefix("pub(crate) ")
                    .or_else(|| rest.strip_prefix("pub "))
                    .unwrap_or(rest);
                let rest = rest.strip_prefix("const ERROR_")?;
                let (_, value) = rest.split_once("&str = \"")?;
                value.split_once('"').map(|(label, _)| label.to_string())
            })
            .collect()
    }

    fn rust_sources(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_sources(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                found.push(path);
            }
        }
    }

    #[test]
    fn the_error_constant_scanner_reads_every_constant_at_any_indentation() {
        let source = "pub const ERROR_A: &str = \"AlphaFailed\";\nconst ERROR_B: &str = \"Beta\";\nconst OTHER: &str = \"Gamma\";\n";
        assert_eq!(error_constants(source), ["AlphaFailed", "Beta"]);
        let nested =
            "fn a() {}\n#[cfg(test)]\nmod tests {\n    pub const ERROR_C: &str = \"Nested\";\n}\n";
        assert_eq!(error_constants(nested), ["Nested"]);
        let crlf = "pub const ERROR_D: &str = \"Delta\";\r\n";
        assert_eq!(error_constants(crlf), ["Delta"]);
        let restricted = "pub(crate) const ERROR_E: &str = \"Epsilon\";\n";
        assert_eq!(error_constants(restricted), ["Epsilon"]);
    }

    fn crate_error_constants() -> Vec<(std::path::PathBuf, String)> {
        let mut files = Vec::new();
        rust_sources(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
            &mut files,
        );
        assert!(files.len() > 50);
        let mut found = Vec::new();
        for file in files {
            for label in error_constants(&std::fs::read_to_string(&file).unwrap()) {
                found.push((file.clone(), label));
            }
        }
        found
    }

    #[test]
    fn every_error_constant_in_the_crate_is_classified() {
        for (file, label) in crate_error_constants() {
            assert!(
                KNOWN_LABELS.iter().any(|(known, _)| *known == label),
                "{} defines the error_type label {label:?}; add it to FailureKind::from_error_type and KNOWN_LABELS",
                file.display()
            );
        }
    }

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
