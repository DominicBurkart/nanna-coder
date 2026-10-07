use crate::impact::change::normalize_path;
use crate::impact::{Access, Change, Evidence, ExtractContext, Extractor};

/// Maps changed file paths to assets through their `owners` globs.
///
/// ```
/// use harness::assets::AssetGraph;
/// use harness::impact::{Change, ImpactAnalyzer, OwnerPathExtractor};
///
/// let graph = AssetGraph::parse("[asset.\"db.users\"]\nkind = \"table\"\nowners = [\"api/src/auth/**\"]\n").unwrap();
/// let analyzer = ImpactAnalyzer::without_extractors(&graph).with_extractor(Box::new(OwnerPathExtractor));
/// assert_eq!(analyzer.analyze(&Change::from_paths(["api/src/auth/login.rs"])).touched, ["db.users"]);
/// assert!(analyzer.analyze(&Change::from_paths(["api/src/orders/mod.rs"])).touched.is_empty());
/// ```
pub struct OwnerPathExtractor;

impl Extractor for OwnerPathExtractor {
    fn name(&self) -> &'static str {
        "paths"
    }

    fn extract(&self, ctx: &ExtractContext<'_>, change: &Change) -> Vec<Evidence> {
        let mut out = Vec::new();
        for file in &change.files {
            for asset in ctx.graph.owners_matching(&file.path) {
                out.push(Evidence::new(
                    self.name(),
                    &file.path,
                    &asset.name,
                    Access::Write,
                    "path is covered by an owner glob or declared site",
                ));
            }
        }
        out
    }
}

/// Maps changed files to assets whose in-code `touches` declarations live in them.
///
/// ```
/// use harness::assets::AssetGraph;
/// use harness::impact::{Change, ImpactAnalyzer, TouchesExtractor};
/// use nanna_effects::Touch;
///
/// let mut graph = AssetGraph::parse("[asset.\"db.orders\"]\nkind = \"table\"\n").unwrap();
/// graph.merge_declarations([Touch::new("db.orders", "api::orders", "api/src/orders.rs", 7)]).unwrap();
/// let analyzer = ImpactAnalyzer::without_extractors(&graph).with_extractor(Box::new(TouchesExtractor));
/// let radius = analyzer.analyze(&Change::from_paths(["api/src/orders.rs"]));
/// assert_eq!(radius.touched, ["db.orders"]);
/// assert!(radius.evidence[0].detail.contains("api::orders"));
/// assert!(analyzer.analyze(&Change::from_paths(["api/src/other.rs"])).touched.is_empty());
/// ```
pub struct TouchesExtractor;

impl Extractor for TouchesExtractor {
    fn name(&self) -> &'static str {
        "touches"
    }

    fn extract(&self, ctx: &ExtractContext<'_>, change: &Change) -> Vec<Evidence> {
        let mut out = Vec::new();
        for file in &change.files {
            for asset in ctx.graph.assets() {
                for site in ctx.graph.sites_of(&asset.name) {
                    if normalize_path(&site.file) == file.path {
                        out.push(Evidence::new(
                            self.name(),
                            &file.path,
                            &asset.name,
                            Access::Write,
                            format!("touches! declared in {}:{}", site.module_path, site.line),
                        ));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::AssetGraph;
    use crate::impact::ImpactAnalyzer;
    use nanna_effects::Touch;

    fn graph() -> AssetGraph {
        let mut graph = AssetGraph::parse(
            "[asset.\"db.users\"]\nkind = \"table\"\nowners = [\"api/src/auth/**\", \"*.sql\"]\n[asset.\"db.orders\"]\nkind = \"table\"\n",
        )
        .unwrap();
        graph
            .merge_declarations([Touch::new(
                "db.orders",
                "api::orders",
                "api/src/orders.rs",
                3,
            )])
            .unwrap();
        graph
    }

    fn run(extractor: Box<dyn Extractor>, paths: &[&str]) -> Vec<String> {
        let graph = graph();
        ImpactAnalyzer::without_extractors(&graph)
            .with_extractor(extractor)
            .analyze(&Change::from_paths(paths.iter().copied()))
            .touched
    }

    #[test]
    fn owner_glob_matches_nested_and_dot_prefixed_paths() {
        assert_eq!(
            run(Box::new(OwnerPathExtractor), &["./api/src/auth/deep/x.rs"]),
            ["db.users"]
        );
        assert_eq!(
            run(Box::new(OwnerPathExtractor), &["api\\src\\auth\\x.rs"]),
            ["db.users"]
        );
    }

    #[test]
    fn owner_glob_ignores_unrelated_and_empty_paths() {
        assert!(run(
            Box::new(OwnerPathExtractor),
            &["api/src/orders/x.rs", "README.md", ""]
        )
        .is_empty());
        assert!(run(Box::new(OwnerPathExtractor), &[]).is_empty());
    }

    #[test]
    fn touches_matches_only_the_declaring_file() {
        assert_eq!(
            run(Box::new(TouchesExtractor), &["api/src/orders.rs"]),
            ["db.orders"]
        );
        assert!(run(Box::new(TouchesExtractor), &["api/src/auth/x.rs"]).is_empty());
    }
}
