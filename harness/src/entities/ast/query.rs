//! Query helpers over [`RustAstEntity`] summaries.
//!
//! Provides small, composable finders for functions, structs, and impl blocks.
//! Matching is case-sensitive substring matching on names, matching the
//! behavior of similar tools (e.g. `ripgrep` default) so callers can drive
//! simple CLI-style discovery over an already-parsed entity.

use super::rust::{FunctionSig, ImplSig, RustAstEntity, StructSig};

/// A read-only query facade over a single [`RustAstEntity`].
///
/// Cheap to construct; intended for short-lived use within a request or a
/// command. For cross-entity queries, iterate over a collection and call the
/// finders per-entity.
#[derive(Debug, Clone, Copy)]
pub struct AstQuery<'a> {
    entity: &'a RustAstEntity,
}

impl<'a> AstQuery<'a> {
    /// Build a new query over `entity`.
    pub fn new(entity: &'a RustAstEntity) -> Self {
        Self { entity }
    }

    /// Find all functions whose name contains `needle` as a substring.
    ///
    /// Returns an empty slice-equivalent `Vec` when no match is found.
    pub fn find_functions(&self, needle: &str) -> Vec<&'a FunctionSig> {
        self.entity
            .summary
            .functions
            .iter()
            .filter(|f| f.name.contains(needle))
            .collect()
    }

    /// Find all structs whose name contains `needle` as a substring.
    pub fn find_structs(&self, needle: &str) -> Vec<&'a StructSig> {
        self.entity
            .summary
            .structs
            .iter()
            .filter(|s| s.name.contains(needle))
            .collect()
    }

    /// Find all impl blocks where either `type_name` or `trait_name`
    /// contains `needle` as a substring.
    pub fn find_impls(&self, needle: &str) -> Vec<&'a ImplSig> {
        self.entity
            .summary
            .impls
            .iter()
            .filter(|i| {
                i.type_name.contains(needle)
                    || i.trait_name.as_ref().is_some_and(|t| t.contains(needle))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::super::rust::{AstSummary, RustAstEntity};
    use super::*;
    use std::path::PathBuf;

    fn fixture() -> RustAstEntity {
        let summary = AstSummary {
            functions: vec![
                FunctionSig {
                    name: "parse_config".to_string(),
                    is_pub: true,
                    is_async: false,
                    line: 1,
                },
                FunctionSig {
                    name: "load".to_string(),
                    is_pub: false,
                    is_async: false,
                    line: 2,
                },
            ],
            structs: vec![
                StructSig {
                    name: "Config".to_string(),
                    is_pub: true,
                    line: 10,
                },
                StructSig {
                    name: "Loader".to_string(),
                    is_pub: false,
                    line: 20,
                },
            ],
            impls: vec![
                ImplSig {
                    type_name: "Config".to_string(),
                    trait_name: None,
                    line: 30,
                },
                ImplSig {
                    type_name: "Loader".to_string(),
                    trait_name: Some("std::fmt::Display".to_string()),
                    line: 40,
                },
            ],
            uses: vec![],
        };
        RustAstEntity::new(PathBuf::from("fake.rs"), summary, "fn parse_config() {}\n")
    }

    #[test]
    fn test_find_functions_match() {
        let entity = fixture();
        let q = AstQuery::new(&entity);
        let hits = q.find_functions("parse");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "parse_config");
    }

    #[test]
    fn test_find_functions_no_match() {
        let entity = fixture();
        let q = AstQuery::new(&entity);
        assert!(q.find_functions("nonexistent_xyz").is_empty());
    }

    #[test]
    fn test_find_structs_match() {
        let entity = fixture();
        let q = AstQuery::new(&entity);
        let hits = q.find_structs("Config");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Config");
        assert!(hits[0].is_pub);

        assert!(q.find_structs("NotThere").is_empty());
    }

    #[test]
    fn test_find_impls_match_by_type() {
        let entity = fixture();
        let q = AstQuery::new(&entity);
        let hits = q.find_impls("Config");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].type_name, "Config");
        assert!(hits[0].trait_name.is_none());
    }

    #[test]
    fn test_find_impls_match_by_trait() {
        let entity = fixture();
        let q = AstQuery::new(&entity);
        let hits = q.find_impls("Display");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].type_name, "Loader");
        assert_eq!(hits[0].trait_name.as_deref(), Some("std::fmt::Display"));
    }

    #[test]
    fn test_find_impls_no_match() {
        let entity = fixture();
        let q = AstQuery::new(&entity);
        assert!(q.find_impls("nowhere").is_empty());
    }
}
