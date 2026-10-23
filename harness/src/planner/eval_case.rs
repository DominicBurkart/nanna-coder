//! Scripted planner scenario used as this issue's evaluation case.
//!
//! Given the fixture catalog `rust-implementer` / `pr-shepherd` /
//! `deployer` that ships under `evals/cases/auditor_spawn/catalog/`
//! (shared with `harness::auditor::eval`) and the task "fix bug X and ship
//! it", a correct planner sequences implementer -> shepherd -> deployer,
//! with each node's `dev_loop` matching its card, and the real
//! [`RuleAuditor`](crate::auditor::RuleAuditor) allows every node.
//!
//! The catalog also ships the inert `auditor` identity; the scripted plan
//! never references it, so it is offered to (mocked) planner calls as one
//! more catalog entry but never chosen.

use std::path::PathBuf;

/// Directory of the identity catalog this scenario plans against.
pub fn catalog_dir() -> PathBuf {
    crate::auditor::eval::default_catalog_dir()
}

/// The task text scripted for this scenario.
pub const TASK: &str = "Fix bug X and ship it.";

/// The scripted planner model reply: implementer, then shepherd, then
/// deployer, wired by `depends_on` in that order.
pub fn scripted_reply() -> &'static str {
    r#"{"nodes":[
        {"id":"implement","identity":"rust-implementer","subtask":"Fix bug X: locate the failing assertion and patch it.","dev_loop":"inner","depends_on":[]},
        {"id":"shepherd","identity":"pr-shepherd","subtask":"Watch the pull request for bug X, keep CI green and address review.","dev_loop":"middle","depends_on":["implement"]},
        {"id":"deploy","identity":"deployer","subtask":"Deploy the merged fix for bug X to the sandbox environment.","dev_loop":"outer","depends_on":["shepherd"]}
    ]}"#
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auditor::{AuditContext, AuditLog, Gate, RuleAuditor, TaskSummary};
    use crate::effects::EffectClass;
    use crate::identity::{DevLoop, IdentityCatalog};
    use crate::planner::model::tests::MockProvider;
    use crate::planner::{
        execute_plan, ModelPlanner, NodeOutcome, PlanNodeId, Planner, SpawnDispatcher,
    };
    use crate::task::{TaskId, TaskResult, TaskStatus};
    use async_trait::async_trait;
    use chrono::Utc;
    use model::ModelProvider;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct SpyDispatcher {
        dispatched: Mutex<Vec<String>>,
    }

    fn completed_status() -> TaskStatus {
        TaskStatus::Completed {
            finished_at: Utc::now(),
            result: TaskResult {
                result_summary: "done".to_string(),
                changes_patch: None,
                format_patch: None,
                files_modified: vec![],
                tool_calls_made: vec![],
                denials: vec![],
                iterations: 1,
                model_used: "mock".to_string(),
            },
        }
    }

    #[async_trait]
    impl SpawnDispatcher for SpyDispatcher {
        async fn dispatch(
            &self,
            allowed: crate::auditor::Allowed,
            _repo_path: std::path::PathBuf,
            _branch: String,
            _model: String,
            _max_iterations: usize,
            _provider: Arc<dyn ModelProvider>,
        ) -> TaskId {
            self.dispatched
                .lock()
                .unwrap()
                .push(allowed.identity().name().to_string());
            TaskId::new()
        }

        async fn wait_terminal(&self, _task_id: &TaskId) -> Option<TaskStatus> {
            Some(completed_status())
        }
    }

    #[tokio::test]
    async fn plans_implementer_then_shepherd_then_deployer_with_correct_loops() {
        let catalog = IdentityCatalog::load(catalog_dir()).unwrap();
        let planner_provider = MockProvider::replying(&[scripted_reply()]);
        let planner = ModelPlanner::new(planner_provider, "mock-planner");
        let plan = planner
            .plan(TASK, &catalog, "a rust monorepo, CI on GitHub Actions")
            .await
            .unwrap();

        let order = plan.topological_order().unwrap();
        assert_eq!(
            order,
            vec![
                PlanNodeId::new("implement"),
                PlanNodeId::new("shepherd"),
                PlanNodeId::new("deploy"),
            ]
        );
        assert_eq!(
            plan.node(&PlanNodeId::new("implement")).unwrap().dev_loop,
            DevLoop::Inner
        );
        assert_eq!(
            plan.node(&PlanNodeId::new("shepherd")).unwrap().dev_loop,
            DevLoop::Middle
        );
        assert_eq!(
            plan.node(&PlanNodeId::new("deploy")).unwrap().dev_loop,
            DevLoop::Outer
        );

        let auditor_identity = catalog.get("auditor").unwrap().clone();
        let context = AuditContext::new(catalog.clone(), auditor_identity)
            .unwrap()
            .with_repo_profile("a rust monorepo, CI on GitHub Actions");
        let gate = Gate::new(RuleAuditor::new(), AuditLog::in_memory());
        let dispatcher = SpyDispatcher::default();
        let parent = TaskSummary::new("task-1", TASK, "github.com/example/repo");
        let dispatch_provider: Arc<dyn ModelProvider> = MockProvider::replying(&[]);
        let execution = execute_plan(
            &plan,
            &gate,
            &context,
            &dispatcher,
            &parent,
            Path::new("/repo"),
            "HEAD",
            dispatch_provider,
        )
        .await
        .unwrap();

        for id in ["implement", "shepherd", "deploy"] {
            match execution.outcome(&PlanNodeId::new(id)).unwrap() {
                NodeOutcome::Dispatched { .. } => {}
                other => panic!("{id}: expected Dispatched, got {other:?}"),
            }
        }
        assert_eq!(execution.task_ids().len(), 3);
        assert_eq!(
            execution.aggregate_effect_class(),
            Some(EffectClass::Sandbox)
        );
        assert_eq!(
            dispatcher.dispatched.lock().unwrap().as_slice(),
            &["rust-implementer", "pr-shepherd", "deployer"]
        );
    }

    #[test]
    fn the_prompt_offers_every_card_by_exact_name_and_instructs_exact_naming() {
        let catalog = IdentityCatalog::load(catalog_dir()).unwrap();
        let messages = ModelPlanner::build_prompt(TASK, &catalog, "profile");
        let system = messages[0].content.clone().unwrap();
        assert!(system.contains("by their exact name"));
        let user = messages[1].content.clone().unwrap();
        for name in ["rust-implementer", "pr-shepherd", "deployer"] {
            assert!(user.contains(name), "{name} missing from prompt");
        }
    }
}
