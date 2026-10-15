//! Deterministic action review: effect ceiling, availability window and
//! coordination lease.

use super::{ActionAuditError, ActionAuditor, ActionContext, ActionReview, ActionVerdict};
use crate::auditor::{Reason, ReasonCode};
use crate::effects::EffectClass;
use crate::leases::{self, LeaseStore};
use crate::windows::{EffectLevel, Window, WindowSet};
use async_trait::async_trait;
use chrono::Duration;
use std::sync::Arc;

/// Name the rule action auditor records against its verdicts.
pub const RULE_ACTION_AUDITOR_NAME: &str = "rule-action-auditor";

pub(crate) const RUN_COMMAND_TOOL: &str = "run_command";

type Inspector = fn(&str, &serde_json::Value) -> Result<(), String>;

/// What [`RuleActionAuditor::evaluate`] reached: a final decision, or a
/// deferral that only a model may resolve.
///
/// Crate-private: [`ModelActionAuditor`](super::ModelActionAuditor) matches
/// on it directly so a rule `Block` can short-circuit the model call, while
/// [`RuleActionAuditor`]'s own [`ActionAuditor`] impl turns a deferral into
/// a `Block` (`Repository`/`Ci`) or an `Escalate` (`Sandbox`/`Production`),
/// since a bare rule auditor has no model to ask.
pub(crate) enum RuleOutcome {
    /// The rules alone decide the outcome.
    Decided(ActionVerdict),
    /// The ceiling (`Repository`/`Ci`) or the window and lease checks
    /// (`Sandbox`/`Production`) passed; only a configured model may allow
    /// the action.
    NeedsModel,
}

/// Cheap, deterministic review used by unit tests, and as the first pass of
/// [`ModelActionAuditor`](super::ModelActionAuditor).
///
/// `None`/`Workspace` calls are always allowed: the inner loop is isolated
/// by container design and needs no gate. `Repository`/`Ci` calls are first
/// compared against the identity's effect ceiling (defense in depth: RBAC
/// should already keep an over-ceiling call out of the tool registry, but
/// the gate checks again); one within the ceiling still needs a model to
/// sign off, so a bare [`RuleActionAuditor`] blocks it with a reason naming
/// the missing model. `Sandbox`/`Production` calls first require the
/// action's target availability window to be open and the coordination
/// lease it needs to be held; a rule auditor used alone can never allow one
/// of these either, so a bare [`RuleActionAuditor`] escalates once window
/// and lease checks pass.
///
/// ```
/// use harness::action_auditor::{ActionAuditor, ActionContext, ActionReview, RuleActionAuditor};
/// use harness::auditor::{ReasonCode, VerdictKind};
/// use harness::effects::EffectClass;
/// use harness::leases::{InMemoryLeaseStore, LeaseContext};
/// use harness::task::TaskId;
/// use harness::windows::WindowSet;
/// use std::sync::Arc;
///
/// # #[tokio::main]
/// # async fn main() {
/// let rules = RuleActionAuditor::new(
///     Arc::new(WindowSet::default()),
///     Arc::new(InMemoryLeaseStore::default()),
///     chrono::Duration::minutes(10),
/// );
///
/// let review = ActionReview {
///     identity: "rust-implementer".to_string(),
///     task_id: TaskId("t1".to_string()),
///     tool: "github_pr_status".to_string(),
///     args: serde_json::json!({}),
///     effect_class: EffectClass::Repository,
///     prior_actions: vec![],
/// };
/// let ctx = ActionContext {
///     max_effect: EffectClass::Repository,
///     window: None,
///     lease: LeaseContext::default(),
///     now: chrono::Utc::now(),
/// };
/// let verdict = rules.review_action(&review, &ctx).await.unwrap();
/// assert_eq!(verdict.kind(), VerdictKind::Block);
///
/// let over_ceiling = ActionContext { max_effect: EffectClass::Workspace, ..ctx };
/// let verdict = rules.review_action(&review, &over_ceiling).await.unwrap();
/// assert_eq!(verdict.kind(), VerdictKind::Block);
/// assert_eq!(verdict.reasons()[0].code, ReasonCode::EffectAboveCeiling);
/// # }
/// ```
pub struct RuleActionAuditor {
    windows: Arc<WindowSet>,
    leases: Arc<dyn LeaseStore>,
    lease_ttl: Duration,
    inspector: Inspector,
}

impl std::fmt::Debug for RuleActionAuditor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuleActionAuditor")
            .field("lease_ttl", &self.lease_ttl)
            .finish()
    }
}

impl RuleActionAuditor {
    /// A rule action auditor checking windows in `windows` and acquiring
    /// leases from `leases` with `lease_ttl`.
    pub fn new(windows: Arc<WindowSet>, leases: Arc<dyn LeaseStore>, lease_ttl: Duration) -> Self {
        Self {
            windows,
            leases,
            lease_ttl,
            inspector: super::commands::inspect,
        }
    }

    /// Replace the `run_command` content inspector, which by default is the
    /// explicit not-implemented stub in `commands`.
    pub fn with_inspector(mut self, inspector: Inspector) -> Self {
        self.inspector = inspector;
        self
    }

    /// Apply every rule to `review` and return the outcome.
    pub(crate) fn evaluate(&self, review: &ActionReview, ctx: &ActionContext<'_>) -> RuleOutcome {
        if review.tool == RUN_COMMAND_TOOL {
            if let Err(detail) = (self.inspector)(&review.tool, &review.args) {
                return RuleOutcome::Decided(ActionVerdict::block(vec![Reason::new(
                    ReasonCode::Other,
                    detail,
                )]));
            }
        }
        match review.effect_class {
            EffectClass::None | EffectClass::Workspace => {
                RuleOutcome::Decided(ActionVerdict::Allow)
            }
            EffectClass::Repository | EffectClass::Ci => match self.ceiling_check(review, ctx) {
                ActionVerdict::Allow => RuleOutcome::NeedsModel,
                verdict => RuleOutcome::Decided(verdict),
            },
            EffectClass::Sandbox => {
                self.window_and_lease_check(review, ctx, leases::Effect::Sandbox)
            }
            EffectClass::Production => {
                self.window_and_lease_check(review, ctx, leases::Effect::Production)
            }
        }
    }

    fn ceiling_check(&self, review: &ActionReview, ctx: &ActionContext<'_>) -> ActionVerdict {
        if review.effect_class > ctx.max_effect {
            let reason = Reason::new(
                ReasonCode::EffectAboveCeiling,
                format!(
                    "`{}` is `{}` but `{}` is capped at `{}`",
                    review.tool, review.effect_class, review.identity, ctx.max_effect
                ),
            );
            return ActionVerdict::block(vec![reason]);
        }
        ActionVerdict::Allow
    }

    fn named_window_block(&self, window: &str, ctx: &ActionContext<'_>) -> Option<ActionVerdict> {
        match self.windows.is_open(window, ctx.now) {
            Ok(true) => None,
            Ok(false) => Some(ActionVerdict::block(vec![Reason::new(
                ReasonCode::WindowClosed,
                format!("window `{window}` is not open"),
            )])),
            Err(e) => Some(ActionVerdict::block(vec![Reason::new(
                ReasonCode::WindowClosed,
                format!("window `{window}`: {e}"),
            )])),
        }
    }

    fn covering_window_block(
        &self,
        review: &ActionReview,
        ctx: &ActionContext<'_>,
    ) -> Option<ActionVerdict> {
        let level = match review.effect_class {
            EffectClass::Sandbox => EffectLevel::Sandbox,
            _ => EffectLevel::Production,
        };
        let covering: Vec<&str> = self.windows.windows_for(level).map(Window::name).collect();
        if covering.is_empty() {
            return Some(ActionVerdict::block(vec![Reason::new(
                ReasonCode::WindowClosed,
                format!("no availability window configured for `{}`", review.tool),
            )]));
        }
        let open = covering
            .iter()
            .any(|name| self.windows.is_open(name, ctx.now).unwrap_or(false));
        if open {
            None
        } else {
            Some(ActionVerdict::block(vec![Reason::new(
                ReasonCode::WindowClosed,
                format!("no `{level}` window is open"),
            )]))
        }
    }

    fn window_and_lease_check(
        &self,
        review: &ActionReview,
        ctx: &ActionContext<'_>,
        effect: leases::Effect,
    ) -> RuleOutcome {
        let blocked = match ctx.window {
            Some(window) => self.named_window_block(window, ctx),
            None => self.covering_window_block(review, ctx),
        };
        if let Some(verdict) = blocked {
            return RuleOutcome::Decided(verdict);
        }
        let names = match leases::required_leases(effect, &ctx.lease) {
            Ok(names) => names,
            Err(e) => {
                let reason = Reason::new(ReasonCode::LeaseUnavailable, e.to_string());
                return RuleOutcome::Decided(ActionVerdict::block(vec![reason]));
            }
        };
        match leases::acquire_all(
            self.leases.as_ref(),
            &names,
            &review.task_id.0,
            self.lease_ttl,
            ctx.now,
        ) {
            Ok(_leases) => RuleOutcome::NeedsModel,
            Err(e) => {
                let reason = Reason::new(ReasonCode::LeaseUnavailable, e.to_string());
                RuleOutcome::Decided(ActionVerdict::block(vec![reason]))
            }
        }
    }
}

#[async_trait]
impl ActionAuditor for RuleActionAuditor {
    fn name(&self) -> &str {
        RULE_ACTION_AUDITOR_NAME
    }

    async fn review_action(
        &self,
        review: &ActionReview,
        context: &ActionContext<'_>,
    ) -> Result<ActionVerdict, ActionAuditError> {
        Ok(match self.evaluate(review, context) {
            RuleOutcome::Decided(verdict) => verdict,
            RuleOutcome::NeedsModel => {
                let reason = Reason::new(
                    ReasonCode::Other,
                    format!(
                        "`{}` is `{}`; a configured model must also review it, and this auditor has no model attached",
                        review.tool, review.effect_class
                    ),
                );
                match review.effect_class {
                    EffectClass::Repository | EffectClass::Ci => ActionVerdict::block(vec![reason]),
                    _ => ActionVerdict::escalate(vec![reason]),
                }
            }
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::auditor::VerdictKind;
    use crate::leases::{InMemoryLeaseStore, LeaseContext, LeaseName, LeaseStore};
    use crate::task::TaskId;
    use chrono::{TimeZone, Utc};

    pub(crate) fn windows() -> Arc<WindowSet> {
        Arc::new(
            WindowSet::parse(
                "[[window]]\nname = \"business-hours\"\ntimezone = \"UTC\"\ndays = [\"mon\", \"tue\", \"wed\", \"thu\", \"fri\"]\nstart = \"09:00\"\nend = \"17:00\"\napplies_to = [\"production\", \"sandbox\"]\n",
            )
            .unwrap(),
        )
    }

    pub(crate) fn auditor() -> RuleActionAuditor {
        RuleActionAuditor::new(
            windows(),
            Arc::new(InMemoryLeaseStore::default()),
            Duration::minutes(10),
        )
    }

    pub(crate) fn review(tool: &str, class: EffectClass) -> ActionReview {
        ActionReview {
            identity: "rust-implementer".to_string(),
            task_id: TaskId("t1".to_string()),
            tool: tool.to_string(),
            args: serde_json::json!({}),
            effect_class: class,
            prior_actions: vec![],
        }
    }

    /// 2026-09-28 is a Monday.
    fn open_monday() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 9, 28, 10, 0, 0).unwrap()
    }

    fn closed_saturday() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 10, 3, 10, 0, 0).unwrap()
    }

    fn ctx<'a>(
        max_effect: EffectClass,
        window: Option<&'a str>,
        lease: LeaseContext<'a>,
    ) -> ActionContext<'a> {
        ActionContext {
            max_effect,
            window,
            lease,
            now: open_monday(),
        }
    }

    #[tokio::test]
    async fn none_and_workspace_are_always_allowed() {
        let rules = auditor();
        for class in [EffectClass::None, EffectClass::Workspace] {
            let review = review("read_file", class);
            let context = ctx(EffectClass::None, None, LeaseContext::default());
            assert_eq!(
                rules.review_action(&review, &context).await.unwrap(),
                ActionVerdict::Allow
            );
        }
    }

    #[tokio::test]
    async fn repository_and_ci_within_ceiling_are_blocked_without_a_model() {
        let rules = auditor();
        for class in [EffectClass::Repository, EffectClass::Ci] {
            let review = review("github_pr_status", class);
            let context = ctx(class, None, LeaseContext::default());
            let verdict = rules.review_action(&review, &context).await.unwrap();
            assert_eq!(verdict.kind(), VerdictKind::Block, "{class}");
            assert!(
                verdict.reasons()[0].detail.contains("model"),
                "{class}: {:?}",
                verdict.reasons()
            );
        }
    }

    #[tokio::test]
    async fn repository_above_ceiling_is_blocked() {
        let rules = auditor();
        let review = review("github_pr_status", EffectClass::Repository);
        let context = ctx(EffectClass::Workspace, None, LeaseContext::default());
        let verdict = rules.review_action(&review, &context).await.unwrap();
        assert_eq!(verdict.kind(), VerdictKind::Block);
        assert_eq!(verdict.reasons()[0].code, ReasonCode::EffectAboveCeiling);
        assert!(verdict.reasons()[0].detail.contains("github_pr_status"));
    }

    #[tokio::test]
    async fn ci_above_ceiling_is_blocked() {
        let rules = auditor();
        let review = review("ci_trigger", EffectClass::Ci);
        let context = ctx(EffectClass::Repository, None, LeaseContext::default());
        let verdict = rules.review_action(&review, &context).await.unwrap();
        assert_eq!(verdict.kind(), VerdictKind::Block);
    }

    fn auditor_over(windows: WindowSet) -> RuleActionAuditor {
        RuleActionAuditor::new(
            Arc::new(windows),
            Arc::new(InMemoryLeaseStore::default()),
            Duration::minutes(10),
        )
    }

    fn sandbox_lease() -> LeaseContext<'static> {
        LeaseContext {
            repo: "example/repo",
            pr: Some(7),
            ..LeaseContext::default()
        }
    }

    fn production_lease() -> LeaseContext<'static> {
        LeaseContext {
            repo: "example/repo",
            environment: Some("production"),
            ..LeaseContext::default()
        }
    }

    #[tokio::test]
    async fn an_unnamed_window_is_resolved_from_the_windows_covering_the_level() {
        let rules = auditor();
        for (class, lease) in [
            (EffectClass::Sandbox, sandbox_lease()),
            (EffectClass::Production, production_lease()),
        ] {
            let review = review("deploy", class);
            let context = ctx(class, None, lease);
            let verdict = rules.review_action(&review, &context).await.unwrap();
            assert_eq!(verdict.kind(), VerdictKind::Escalate, "{class}");
        }
    }

    #[tokio::test]
    async fn an_unnamed_window_that_is_closed_blocks_both_levels() {
        let rules = auditor();
        for (class, lease) in [
            (EffectClass::Sandbox, sandbox_lease()),
            (EffectClass::Production, production_lease()),
        ] {
            let review = review("deploy", class);
            let mut context = ctx(class, None, lease);
            context.now = closed_saturday();
            let verdict = rules.review_action(&review, &context).await.unwrap();
            assert_eq!(verdict.kind(), VerdictKind::Block, "{class}");
            assert_eq!(verdict.reasons()[0].code, ReasonCode::WindowClosed);
            assert!(verdict.reasons()[0].detail.contains(&class.to_string()));
        }
    }

    #[tokio::test]
    async fn an_unnamed_window_with_no_window_covering_the_level_blocks() {
        for windows in [
            WindowSet::default(),
            WindowSet::parse(
                "[[window]]\nname = \"prod-only\"\ntimezone = \"UTC\"\ndays = [\"mon\"]\nstart = \"00:00\"\nend = \"23:59\"\napplies_to = [\"production\"]\n",
            )
            .unwrap(),
        ] {
            let rules = auditor_over(windows);
            let review = review("sandbox_deploy", EffectClass::Sandbox);
            let context = ctx(EffectClass::Sandbox, None, sandbox_lease());
            let verdict = rules.review_action(&review, &context).await.unwrap();
            assert_eq!(verdict.kind(), VerdictKind::Block);
            assert_eq!(verdict.reasons()[0].code, ReasonCode::WindowClosed);
            assert!(verdict.reasons()[0]
                .detail
                .contains("no availability window configured"));
        }
    }

    #[tokio::test]
    async fn sandbox_outside_its_window_is_blocked_before_reaching_the_tool() {
        let rules = auditor();
        let review = review("sandbox_deploy", EffectClass::Sandbox);
        let lease = LeaseContext {
            repo: "example/repo",
            pr: Some(7),
            ..LeaseContext::default()
        };
        let mut context = ctx(EffectClass::Sandbox, Some("business-hours"), lease);
        context.now = closed_saturday();
        let verdict = rules.review_action(&review, &context).await.unwrap();
        assert_eq!(verdict.kind(), VerdictKind::Block);
        assert_eq!(verdict.reasons()[0].code, ReasonCode::WindowClosed);
        assert!(verdict.reasons()[0].detail.contains("business-hours"));
    }

    #[tokio::test]
    async fn sandbox_with_an_unknown_window_is_blocked() {
        let rules = auditor();
        let review = review("sandbox_deploy", EffectClass::Sandbox);
        let lease = LeaseContext {
            repo: "example/repo",
            pr: Some(7),
            ..LeaseContext::default()
        };
        let context = ctx(EffectClass::Sandbox, Some("no-such-window"), lease);
        let verdict = rules.review_action(&review, &context).await.unwrap();
        assert_eq!(verdict.kind(), VerdictKind::Block);
        assert_eq!(verdict.reasons()[0].code, ReasonCode::WindowClosed);
    }

    #[tokio::test]
    async fn sandbox_with_missing_lease_context_is_blocked() {
        let rules = auditor();
        let review = review("sandbox_deploy", EffectClass::Sandbox);
        let lease = LeaseContext {
            repo: "example/repo",
            ..LeaseContext::default()
        };
        let context = ctx(EffectClass::Sandbox, Some("business-hours"), lease);
        let verdict = rules.review_action(&review, &context).await.unwrap();
        assert_eq!(verdict.kind(), VerdictKind::Block);
        assert_eq!(verdict.reasons()[0].code, ReasonCode::LeaseUnavailable);
    }

    #[tokio::test]
    async fn production_without_its_lease_held_is_blocked() {
        let store = Arc::new(InMemoryLeaseStore::default());
        store
            .acquire(
                &LeaseName::deploy("example/repo", "prod"),
                "other-task",
                Duration::minutes(10),
                open_monday(),
            )
            .unwrap();
        let rules = RuleActionAuditor::new(windows(), store, Duration::minutes(10));
        let review = review("prod_rollout", EffectClass::Production);
        let lease = LeaseContext {
            repo: "example/repo",
            environment: Some("prod"),
            ..LeaseContext::default()
        };
        let context = ctx(EffectClass::Production, Some("business-hours"), lease);
        let verdict = rules.review_action(&review, &context).await.unwrap();
        assert_eq!(verdict.kind(), VerdictKind::Block);
        assert_eq!(verdict.reasons()[0].code, ReasonCode::LeaseUnavailable);
    }

    #[tokio::test]
    async fn sandbox_with_window_open_and_lease_held_escalates_without_a_model() {
        let rules = auditor();
        let review = review("sandbox_deploy", EffectClass::Sandbox);
        let lease = LeaseContext {
            repo: "example/repo",
            pr: Some(7),
            ..LeaseContext::default()
        };
        let context = ctx(EffectClass::Sandbox, Some("business-hours"), lease);
        let verdict = rules.review_action(&review, &context).await.unwrap();
        assert_eq!(verdict.kind(), VerdictKind::Escalate);
    }

    #[tokio::test]
    async fn re_acquiring_the_same_task_holder_renews_rather_than_blocks() {
        let rules = auditor();
        let review = review("sandbox_deploy", EffectClass::Sandbox);
        let lease = LeaseContext {
            repo: "example/repo",
            pr: Some(7),
            ..LeaseContext::default()
        };
        let context = ctx(EffectClass::Sandbox, Some("business-hours"), lease);
        let first = rules.review_action(&review, &context).await.unwrap();
        let second = rules.review_action(&review, &context).await.unwrap();
        assert_eq!(first.kind(), VerdictKind::Escalate);
        assert_eq!(second.kind(), VerdictKind::Escalate);
    }

    static COUNTED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    fn counting_inspector(_tool: &str, _args: &serde_json::Value) -> Result<(), String> {
        COUNTED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn rejecting_inspector(tool: &str, args: &serde_json::Value) -> Result<(), String> {
        Err(format!("{tool} refused for {args}"))
    }

    fn passing_inspector(_tool: &str, _args: &serde_json::Value) -> Result<(), String> {
        Ok(())
    }

    fn run_command_review() -> ActionReview {
        let mut review = review(RUN_COMMAND_TOOL, EffectClass::Workspace);
        review.args = serde_json::json!({ "command": "echo hi" });
        review
    }

    #[tokio::test]
    async fn a_rejecting_inspector_blocks_run_command() {
        let rules = auditor().with_inspector(rejecting_inspector);
        let context = ctx(EffectClass::Workspace, None, LeaseContext::default());
        let verdict = rules
            .review_action(&run_command_review(), &context)
            .await
            .unwrap();
        assert_eq!(verdict.kind(), VerdictKind::Block);
        assert!(verdict.reasons()[0].detail.contains("run_command refused"));
        assert!(verdict.reasons()[0].detail.contains("echo hi"));
    }

    #[tokio::test]
    async fn a_passing_inspector_lets_run_command_through() {
        let rules = auditor().with_inspector(passing_inspector);
        let context = ctx(EffectClass::Workspace, None, LeaseContext::default());
        let verdict = rules
            .review_action(&run_command_review(), &context)
            .await
            .unwrap();
        assert_eq!(verdict, ActionVerdict::Allow);
    }

    #[tokio::test]
    async fn every_run_command_evaluation_calls_the_inspector_and_no_other_tool_does() {
        let rules = auditor().with_inspector(counting_inspector);
        let context = ctx(EffectClass::Workspace, None, LeaseContext::default());
        let before = COUNTED.load(std::sync::atomic::Ordering::SeqCst);
        for _ in 0..3 {
            rules
                .review_action(&run_command_review(), &context)
                .await
                .unwrap();
        }
        rules
            .review_action(&review("write_file", EffectClass::Workspace), &context)
            .await
            .unwrap();
        assert_eq!(
            COUNTED.load(std::sync::atomic::Ordering::SeqCst) - before,
            3
        );
    }

    #[tokio::test]
    async fn the_default_inspector_is_the_named_not_implemented_stub() {
        let rules = auditor();
        let context = ctx(EffectClass::Workspace, None, LeaseContext::default());
        let verdict = rules
            .review_action(&run_command_review(), &context)
            .await
            .unwrap();
        assert_eq!(verdict, ActionVerdict::Allow);
    }

    #[test]
    fn debug_output_omits_stores_but_names_the_field_present() {
        let rules = auditor();
        assert!(format!("{rules:?}").contains("RuleActionAuditor"));
    }

    #[tokio::test]
    async fn name_is_the_stable_constant() {
        assert_eq!(auditor().name(), RULE_ACTION_AUDITOR_NAME);
    }
}
