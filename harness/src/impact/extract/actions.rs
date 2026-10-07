use crate::assets::AssetKind;
use crate::impact::{Access, Action, Change, Evidence, ExtractContext, Extractor};

/// Maps actions to assets: a sandbox deploy or rollout of environment `E`
/// touches every endpoint and job asset (the deployed, served assets); a CI
/// trigger touches nothing.
///
/// When the analyzer has an environment list (see
/// [`ImpactAnalyzer::with_environments`](crate::impact::ImpactAnalyzer::with_environments))
/// an action naming an environment outside it touches nothing.
///
/// ```
/// use harness::assets::AssetGraph;
/// use harness::impact::{Action, ActionExtractor, Change, ImpactAnalyzer};
///
/// let graph = AssetGraph::parse("[asset.\"db.a\"]\nkind = \"table\"\n[asset.\"http.GET /a\"]\nkind = \"endpoint\"\n").unwrap();
/// let analyzer = ImpactAnalyzer::without_extractors(&graph)
///     .with_extractor(Box::new(ActionExtractor))
///     .with_environments(["sandbox"]);
/// let deploy = Change::from_actions([Action::SandboxDeploy { environment: "sandbox".into() }]);
/// assert_eq!(analyzer.analyze(&deploy).touched, ["http.GET /a"]);
/// assert!(analyzer.analyze(&Change::from_actions([Action::CiTrigger])).is_empty());
/// let elsewhere = Change::from_actions([Action::Rollout { environment: "mars".into() }]);
/// assert!(analyzer.analyze(&elsewhere).is_empty());
/// ```
pub struct ActionExtractor;

impl Extractor for ActionExtractor {
    fn name(&self) -> &'static str {
        "actions"
    }

    fn extract(&self, ctx: &ExtractContext<'_>, change: &Change) -> Vec<Evidence> {
        let mut out = Vec::new();
        for action in &change.actions {
            let environment = match action {
                Action::SandboxDeploy { environment } | Action::Rollout { environment } => {
                    environment
                }
                Action::CiTrigger => continue,
            };
            if ctx
                .environments
                .is_some_and(|known| !known.iter().any(|e| e == environment))
            {
                continue;
            }
            for asset in ctx
                .graph
                .assets()
                .filter(|a| matches!(a.kind, AssetKind::Endpoint | AssetKind::Job))
            {
                out.push(Evidence::new(
                    self.name(),
                    action.label(),
                    &asset.name,
                    Access::Write,
                    format!("served by environment {environment}"),
                ));
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

    fn graph() -> AssetGraph {
        AssetGraph::parse(
            "[asset.\"db.a\"]\nkind = \"table\"\n[asset.\"http.GET /a\"]\nkind = \"endpoint\"\n[asset.\"job.j\"]\nkind = \"job\"\n[asset.\"ext.p\"]\nkind = \"external\"\n",
        )
        .unwrap()
    }

    fn deploy(environment: &str) -> Change {
        Change::from_actions([Action::SandboxDeploy {
            environment: environment.into(),
        }])
    }

    #[test]
    fn deploy_touches_served_assets_only() {
        let graph = graph();
        let radius = ImpactAnalyzer::without_extractors(&graph)
            .with_extractor(Box::new(ActionExtractor))
            .analyze(&deploy("sandbox"));
        assert_eq!(radius.touched, ["http.GET /a", "job.j"]);
        assert!(radius.evidence[0].subject.starts_with("sandbox_deploy("));
    }

    #[test]
    fn rollout_behaves_like_deploy() {
        let graph = graph();
        let analyzer =
            ImpactAnalyzer::without_extractors(&graph).with_extractor(Box::new(ActionExtractor));
        let radius = analyzer.analyze(&Change::from_actions([Action::Rollout {
            environment: "production".into(),
        }]));
        assert_eq!(radius.touched.len(), 2);
    }

    #[test]
    fn ci_trigger_touches_nothing_and_unknown_environment_touches_nothing() {
        let graph = graph();
        let analyzer = ImpactAnalyzer::without_extractors(&graph)
            .with_extractor(Box::new(ActionExtractor))
            .with_environments(["sandbox", "staging"]);
        assert!(analyzer
            .analyze(&Change::from_actions([Action::CiTrigger]))
            .is_empty());
        assert!(analyzer.analyze(&deploy("production")).is_empty());
        assert_eq!(analyzer.analyze(&deploy("staging")).touched.len(), 2);
    }

    #[test]
    fn empty_graph_and_no_actions_are_empty() {
        let graph = AssetGraph::new();
        let analyzer =
            ImpactAnalyzer::without_extractors(&graph).with_extractor(Box::new(ActionExtractor));
        assert!(analyzer.analyze(&deploy("sandbox")).is_empty());
        assert!(analyzer.analyze(&Change::default()).is_empty());
    }
}
