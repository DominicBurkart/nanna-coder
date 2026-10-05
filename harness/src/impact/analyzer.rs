use super::change::Change;
use super::model::{Access, AssetId, BlastRadius, Evidence, Weight};
use crate::assets::AssetGraph;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Everything an [`Extractor`] may consult besides the change itself.
pub struct ExtractContext<'a> {
    pub graph: &'a AssetGraph,
    pub environments: Option<&'a [String]>,
}

/// Maps one kind of input to the assets it reaches.
///
/// Implement this to teach the analyzer a new input; evidence naming assets
/// that are not in the graph is discarded by the analyzer.
///
/// ```
/// use harness::assets::AssetGraph;
/// use harness::impact::{Access, Change, Evidence, ExtractContext, Extractor, ImpactAnalyzer};
///
/// struct Everything;
///
/// impl Extractor for Everything {
///     fn name(&self) -> &'static str {
///         "everything"
///     }
///
///     fn extract(&self, ctx: &ExtractContext<'_>, _change: &Change) -> Vec<Evidence> {
///         ctx.graph
///             .assets()
///             .map(|a| Evidence::new("everything", "all", a.name.clone(), Access::Write, "always"))
///             .collect()
///     }
/// }
///
/// let graph = AssetGraph::parse("[asset.\"db.a\"]\nkind = \"table\"\n").unwrap();
/// let analyzer = ImpactAnalyzer::without_extractors(&graph).with_extractor(Box::new(Everything));
/// assert_eq!(analyzer.analyze(&Change::default()).touched, ["db.a"]);
/// ```
pub trait Extractor: Send + Sync {
    fn name(&self) -> &'static str;

    fn extract(&self, ctx: &ExtractContext<'_>, change: &Change) -> Vec<Evidence>;
}

/// Computes [`BlastRadius`] for changes and actions against an asset graph.
///
/// ```
/// use harness::assets::AssetGraph;
/// use harness::impact::{Change, ImpactAnalyzer};
///
/// let graph = AssetGraph::parse(r#"
/// [concern.revenue]
/// weight = 10
/// [asset."db.orders"]
/// kind = "table"
/// concerns = ["revenue"]
/// owners = ["api/src/orders/**"]
/// "#).unwrap();
/// let radius = ImpactAnalyzer::new(&graph).analyze(&Change::from_paths(["api/src/orders/mod.rs"]));
/// assert_eq!(radius.touched, ["db.orders"]);
/// assert_eq!(radius.concerns["revenue"], 10);
/// assert_eq!(radius.score, 10);
/// ```
pub struct ImpactAnalyzer<'g> {
    graph: &'g AssetGraph,
    extractors: Vec<Box<dyn Extractor>>,
    environments: Option<Vec<String>>,
}

impl<'g> ImpactAnalyzer<'g> {
    /// An analyzer with every built-in extractor.
    pub fn new(graph: &'g AssetGraph) -> Self {
        let mut analyzer = Self::without_extractors(graph);
        analyzer.extractors = super::extract::builtin();
        analyzer
    }

    pub fn without_extractors(graph: &'g AssetGraph) -> Self {
        Self {
            graph,
            extractors: Vec::new(),
            environments: None,
        }
    }

    pub fn with_extractor(mut self, extractor: Box<dyn Extractor>) -> Self {
        self.extractors.push(extractor);
        self
    }

    /// Restrict deploy and rollout actions to these environments; an action
    /// naming another environment touches nothing. Unset, every environment
    /// is accepted.
    pub fn with_environments<I, S>(mut self, environments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.environments = Some(environments.into_iter().map(Into::into).collect());
        self
    }

    pub fn graph(&self) -> &AssetGraph {
        self.graph
    }

    /// Run every extractor over `change` and score the result.
    pub fn analyze(&self, change: &Change) -> BlastRadius {
        let ctx = ExtractContext {
            graph: self.graph,
            environments: self.environments.as_deref(),
        };
        let mut evidence: BTreeSet<Evidence> = BTreeSet::new();
        for extractor in &self.extractors {
            evidence.extend(
                extractor
                    .extract(&ctx, change)
                    .into_iter()
                    .filter(|item| self.graph.asset(&item.asset).is_some()),
            );
        }
        let touched: BTreeSet<AssetId> = evidence
            .iter()
            .filter(|item| item.access == Access::Write)
            .map(|item| item.asset.clone())
            .collect();
        BlastRadius::compute(self.graph, touched, evidence.into_iter().collect())
    }

    /// Score a set of already-known touched assets, without evidence.
    ///
    /// ```
    /// use harness::assets::AssetGraph;
    /// use harness::impact::ImpactAnalyzer;
    ///
    /// let graph = AssetGraph::parse("[asset.\"db.a\"]\nkind = \"table\"\n").unwrap();
    /// let radius = ImpactAnalyzer::new(&graph).radius_of_assets(["db.a", "db.unknown"]);
    /// assert_eq!(radius.touched, ["db.a"]);
    /// ```
    pub fn radius_of_assets<I, S>(&self, assets: I) -> BlastRadius
    where
        I: IntoIterator<Item = S>,
        S: Into<AssetId>,
    {
        let touched = assets
            .into_iter()
            .map(Into::into)
            .filter(|name| self.graph.asset(name).is_some())
            .collect();
        BlastRadius::compute(self.graph, touched, Vec::new())
    }
}

/// Divisor exponent applied per level of depth; see [`BlastRadius::compute`].
pub const DEPTH_DISCOUNT_SHIFT: u32 = 1;

impl BlastRadius {
    /// Score `touched` assets and everything that transitively reads them.
    ///
    /// The score is the sum, over every touched and downstream asset, of the
    /// weights of that asset's distinct concerns, where an asset at distance
    /// `d` from the nearest touched asset along `reads` edges contributes its
    /// weight shifted right by `d * DEPTH_DISCOUNT_SHIFT` bits (halved per
    /// level, rounding down, so zero from depth 4 for weights below 16).
    /// Touched assets have depth 0. Additions saturate at `u32::MAX`.
    ///
    /// ```
    /// use harness::assets::AssetGraph;
    /// use harness::impact::BlastRadius;
    /// use std::collections::BTreeSet;
    ///
    /// let graph = AssetGraph::parse(r#"
    /// [concern.content]
    /// weight = 3
    /// [concern.auth]
    /// weight = 9
    /// [asset."db.a"]
    /// kind = "table"
    /// concerns = ["content"]
    /// [asset."http.b"]
    /// kind = "endpoint"
    /// concerns = ["auth"]
    /// reads = ["db.a"]
    /// [asset."job.c"]
    /// kind = "job"
    /// concerns = ["auth"]
    /// reads = ["http.b"]
    /// "#).unwrap();
    /// let radius = BlastRadius::compute(&graph, BTreeSet::from(["db.a".to_string()]), Vec::new());
    /// assert_eq!(radius.downstream, ["http.b", "job.c"]);
    /// assert_eq!(radius.score, 3 + (9 >> 1) + (9 >> 2));
    /// ```
    pub fn compute(
        graph: &AssetGraph,
        touched: BTreeSet<AssetId>,
        evidence: Vec<Evidence>,
    ) -> BlastRadius {
        let depths = downstream_depths(graph, &touched);
        let mut score: u32 = 0;
        let mut concerns: BTreeMap<String, Weight> = BTreeMap::new();
        let reached = touched
            .iter()
            .map(|name| (name, 0u32))
            .chain(depths.iter().map(|(name, depth)| (name, *depth)));
        for (name, depth) in reached {
            let Some(asset) = graph.asset(name) else {
                continue;
            };
            let names: BTreeSet<&String> = asset.concerns.iter().collect();
            let mut weight: u32 = 0;
            for concern_name in names {
                if let Some(concern) = graph.concern(concern_name) {
                    weight = weight.saturating_add(concern.weight);
                    concerns.insert(concern.name.clone(), concern.weight);
                }
            }
            let shift = depth.saturating_mul(DEPTH_DISCOUNT_SHIFT);
            score = score.saturating_add(weight.checked_shr(shift).unwrap_or(0));
        }
        let mut evidence = evidence;
        evidence.sort();
        evidence.dedup();
        BlastRadius {
            touched: touched.into_iter().collect(),
            downstream: depths.into_keys().collect(),
            concerns,
            score,
            evidence,
        }
    }
}

fn downstream_depths(graph: &AssetGraph, touched: &BTreeSet<AssetId>) -> BTreeMap<AssetId, u32> {
    let mut readers: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for asset in graph.assets() {
        for read in &asset.reads {
            readers.entry(read.as_str()).or_default().push(&asset.name);
        }
    }
    let mut depths: BTreeMap<AssetId, u32> = BTreeMap::new();
    let mut queue: VecDeque<(&str, u32)> = touched.iter().map(|name| (name.as_str(), 0)).collect();
    while let Some((current, depth)) = queue.pop_front() {
        for &reader in readers.get(current).into_iter().flatten() {
            if !touched.contains(reader) && !depths.contains_key(reader) {
                depths.insert(reader.to_string(), depth + 1);
                queue.push_back((reader, depth + 1));
            }
        }
    }
    depths
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assets::{Asset, AssetKind, Concern};
    use proptest::prelude::*;

    const MANIFEST: &str = r#"
[concern.revenue]
weight = 10
[concern.availability]
weight = 6

[asset."db.orders"]
kind = "table"
concerns = ["revenue"]

[asset."http.POST /checkout"]
kind = "endpoint"
concerns = ["revenue", "availability"]
reads = ["db.orders"]

[asset."job.invoice"]
kind = "job"
concerns = ["revenue"]
reads = ["db.orders"]

[asset."ext.pay"]
kind = "external"
concerns = ["revenue"]
"#;

    fn graph() -> AssetGraph {
        AssetGraph::parse(MANIFEST).unwrap()
    }

    fn touched(names: &[&str]) -> BTreeSet<AssetId> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn empty_touched_scores_zero() {
        let radius = BlastRadius::compute(&graph(), BTreeSet::new(), Vec::new());
        assert_eq!(radius, BlastRadius::default());
    }

    #[test]
    fn downstream_is_discounted_by_depth() {
        let radius = BlastRadius::compute(&graph(), touched(&["db.orders"]), Vec::new());
        assert_eq!(radius.touched, ["db.orders"]);
        assert_eq!(radius.downstream, ["http.POST /checkout", "job.invoice"]);
        assert_eq!(radius.score, 10 + (16 >> 1) + (10 >> 1));
        assert_eq!(radius.concerns["revenue"], 10);
        assert_eq!(radius.concerns["availability"], 6);
    }

    #[test]
    fn leaf_change_does_not_reach_upstream() {
        let radius = BlastRadius::compute(&graph(), touched(&["http.POST /checkout"]), Vec::new());
        assert!(radius.downstream.is_empty());
        assert_eq!(radius.score, 16);
    }

    #[test]
    fn touched_asset_is_never_also_downstream() {
        let radius =
            BlastRadius::compute(&graph(), touched(&["db.orders", "job.invoice"]), Vec::new());
        assert_eq!(radius.downstream, ["http.POST /checkout"]);
        assert_eq!(radius.score, 10 + 10 + 8);
    }

    #[test]
    fn cycles_terminate() {
        let mut g = AssetGraph::new();
        for name in ["a", "b", "c"] {
            let mut asset = Asset::new(name, AssetKind::Job);
            asset.reads = vec!["a".into(), "b".into(), "c".into()];
            g.insert_asset(asset).unwrap();
        }
        let radius = BlastRadius::compute(&g, touched(&["a"]), Vec::new());
        assert_eq!(radius.downstream, ["b", "c"]);
    }

    #[test]
    fn depth_beyond_word_width_contributes_zero_without_panicking() {
        let mut g = AssetGraph::new();
        g.insert_concern(Concern {
            name: "c".into(),
            weight: u32::MAX,
            description: None,
        })
        .unwrap();
        for i in 0..40 {
            let mut asset = Asset::new(format!("n{i}"), AssetKind::Job);
            asset.concerns = vec!["c".into()];
            if i > 0 {
                asset.reads = vec![format!("n{}", i - 1)];
            }
            g.insert_asset(asset).unwrap();
        }
        let radius = BlastRadius::compute(&g, touched(&["n0"]), Vec::new());
        assert_eq!(radius.downstream.len(), 39);
        assert!(radius.score >= u32::MAX >> 1);
    }

    #[test]
    fn scores_saturate() {
        let mut g = AssetGraph::new();
        g.insert_concern(Concern {
            name: "c".into(),
            weight: u32::MAX,
            description: None,
        })
        .unwrap();
        for name in ["a", "b"] {
            let mut asset = Asset::new(name, AssetKind::Table);
            asset.concerns = vec!["c".into()];
            g.insert_asset(asset).unwrap();
        }
        let radius = BlastRadius::compute(&g, touched(&["a", "b"]), Vec::new());
        assert_eq!(radius.score, u32::MAX);
    }

    #[test]
    fn unknown_assets_are_dropped_by_radius_of_assets() {
        let g = graph();
        let radius = ImpactAnalyzer::new(&g).radius_of_assets(["ghost"]);
        assert!(radius.is_empty());
    }

    #[test]
    fn analyzer_output_is_deterministic() {
        let g = graph();
        let a = ImpactAnalyzer::new(&g).radius_of_assets(["job.invoice", "db.orders"]);
        let b = ImpactAnalyzer::new(&g).radius_of_assets(["db.orders", "job.invoice"]);
        assert_eq!(a, b);
    }

    const NODES: usize = 6;

    fn edge_set() -> impl Strategy<Value = BTreeSet<(usize, usize)>> {
        prop::collection::btree_set((0..NODES, 0..NODES), 0..24)
    }

    fn build(weights: &[Vec<usize>], edges: &BTreeSet<(usize, usize)>) -> AssetGraph {
        let mut g = AssetGraph::new();
        for (i, weight) in [1u32, 2, 4, 8, 16, 32].into_iter().enumerate() {
            g.insert_concern(Concern {
                name: format!("c{i}"),
                weight,
                description: None,
            })
            .unwrap();
        }
        for (i, concerns) in weights.iter().enumerate() {
            let mut asset = Asset::new(format!("n{i}"), AssetKind::Job);
            let distinct: BTreeSet<usize> = concerns.iter().copied().collect();
            asset.concerns = distinct.iter().map(|c| format!("c{c}")).collect();
            asset.reads = edges
                .iter()
                .filter(|(from, _)| *from == i)
                .map(|(_, to)| format!("n{to}"))
                .collect();
            g.insert_asset(asset).unwrap();
        }
        g
    }

    fn concerns_strategy() -> impl Strategy<Value = Vec<Vec<usize>>> {
        prop::collection::vec(prop::collection::vec(0..6usize, 0..4), NODES)
    }

    proptest! {
        #[test]
        fn adding_an_edge_never_lowers_the_score(
            concerns in concerns_strategy(),
            edges in edge_set(),
            extra in (0..NODES, 0..NODES),
            seeds in prop::collection::btree_set(0..NODES, 0..4),
        ) {
            let touched: BTreeSet<AssetId> = seeds.iter().map(|i| format!("n{i}")).collect();
            let before = BlastRadius::compute(&build(&concerns, &edges), touched.clone(), Vec::new());
            let mut more = edges.clone();
            more.insert(extra);
            let after = BlastRadius::compute(&build(&concerns, &more), touched, Vec::new());
            prop_assert!(after.score >= before.score);
            prop_assert!(after.downstream.len() >= before.downstream.len());
            for name in &before.downstream {
                prop_assert!(after.downstream.contains(name));
            }
        }

        #[test]
        fn touching_more_assets_never_lowers_the_score(
            concerns in concerns_strategy(),
            edges in edge_set(),
            seeds in prop::collection::btree_set(0..NODES, 0..4),
            extra in 0..NODES,
        ) {
            let g = build(&concerns, &edges);
            let small: BTreeSet<AssetId> = seeds.iter().map(|i| format!("n{i}")).collect();
            let mut large = small.clone();
            large.insert(format!("n{extra}"));
            let before = BlastRadius::compute(&g, small, Vec::new());
            let after = BlastRadius::compute(&g, large, Vec::new());
            prop_assert!(after.score >= before.score);
        }

        #[test]
        fn computation_terminates_and_partitions_assets(
            concerns in concerns_strategy(),
            edges in edge_set(),
            seeds in prop::collection::btree_set(0..NODES, 0..NODES),
        ) {
            let g = build(&concerns, &edges);
            let touched: BTreeSet<AssetId> = seeds.iter().map(|i| format!("n{i}")).collect();
            let radius = BlastRadius::compute(&g, touched.clone(), Vec::new());
            prop_assert!(radius.touched.len() + radius.downstream.len() <= NODES);
            for name in &radius.downstream {
                prop_assert!(!touched.contains(name));
            }
            let closure: BTreeSet<String> = touched
                .iter()
                .flat_map(|name| g.dependents_of(name))
                .filter(|name| !touched.contains(name))
                .collect();
            prop_assert_eq!(radius.downstream.iter().cloned().collect::<BTreeSet<_>>(), closure);
        }
    }
}
