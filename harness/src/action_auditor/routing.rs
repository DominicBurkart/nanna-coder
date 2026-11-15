//! Tool-name dispatch between an outbound message router and the default
//! action auditor.

use super::{ActionAuditError, ActionAuditor, ActionContext, ActionReview, ActionVerdict};
use crate::auditor::{Reason, ReasonCode};
use async_trait::async_trait;
use std::sync::{Arc, Weak};

/// The tools whose arguments carry agent-authored content to other agents.
///
/// [`ToolRegistry::execute`](crate::tools::ToolRegistry::execute) reviews
/// these at any effect class, and [`RoutingActionAuditor`] fails them closed
/// when its router is gone. The list is fixed and pinned by a test.
///
/// ```
/// use harness::action_auditor::MESSAGE_TOOLS;
/// assert_eq!(MESSAGE_TOOLS, ["msg_send", "session_set_note", "sessions_query"]);
/// ```
pub const MESSAGE_TOOLS: [&str; 3] = ["msg_send", "session_set_note", "sessions_query"];

/// Whether `tool` is one of the [`MESSAGE_TOOLS`].
///
/// ```
/// use harness::action_auditor::is_message_tool;
/// assert!(is_message_tool("msg_send"));
/// assert!(!is_message_tool("write_file"));
/// ```
pub fn is_message_tool(tool: &str) -> bool {
    MESSAGE_TOOLS.contains(&tool)
}

/// The seam a message hub implements to review its own tools.
///
/// A router reviews purely: [`review`](Self::review) must not write.
/// [`on_quarantine`](Self::on_quarantine) is the single permitted write, and
/// is called by [`RoutingActionAuditor`] only when `review` escalated.
///
/// ```
/// use async_trait::async_trait;
/// use harness::action_auditor::{
///     ActionAuditError, ActionContext, ActionReview, ActionVerdict, OutboundRouter,
/// };
/// use harness::auditor::Reason;
///
/// struct DenyAll;
///
/// #[async_trait]
/// impl OutboundRouter for DenyAll {
///     fn handles(&self, tool: &str) -> bool {
///         tool == "msg_send"
///     }
///     async fn review(
///         &self,
///         _review: &ActionReview,
///         _context: &ActionContext<'_>,
///     ) -> Result<ActionVerdict, ActionAuditError> {
///         Ok(ActionVerdict::block(vec![]))
///     }
///     async fn on_quarantine(
///         &self,
///         _review: &ActionReview,
///         _reasons: &[Reason],
///     ) -> Result<(), ActionAuditError> {
///         Ok(())
///     }
/// }
/// assert!(DenyAll.handles("msg_send"));
/// ```
#[async_trait]
pub trait OutboundRouter: Send + Sync {
    /// Whether this router reviews calls to `tool`.
    fn handles(&self, tool: &str) -> bool;

    /// Review a call to a tool for which [`handles`](Self::handles) is true.
    async fn review(
        &self,
        review: &ActionReview,
        context: &ActionContext<'_>,
    ) -> Result<ActionVerdict, ActionAuditError>;

    /// Record that `review` was escalated for `reasons`.
    async fn on_quarantine(
        &self,
        review: &ActionReview,
        reasons: &[Reason],
    ) -> Result<(), ActionAuditError>;
}

/// Sends the router's tools to an [`OutboundRouter`] and every other tool to
/// a default [`ActionAuditor`], unchanged.
///
/// Routing is decided by tool name alone: a [`MESSAGE_TOOLS`] call goes to
/// the router and is blocked when the router has been dropped or does not
/// [`handle`](OutboundRouter::handles) it, never to the default; every other
/// call goes to the default whatever the router says. The router is held as
/// a [`Weak`] to break the ownership cycle between the hub and the registries
/// that carry this auditor. An escalation calls
/// [`on_quarantine`](OutboundRouter::on_quarantine) once; if that fails the
/// verdict is still an escalation, carrying an extra reason line and an error
/// log entry.
///
/// ```
/// use harness::action_auditor::{RoutingActionAuditor, RuleActionAuditor, ActionAuditor};
/// use harness::leases::InMemoryLeaseStore;
/// use harness::windows::WindowSet;
/// use std::sync::Arc;
///
/// let default = RuleActionAuditor::new(
///     Arc::new(WindowSet::default()),
///     Arc::new(InMemoryLeaseStore::default()),
///     chrono::Duration::minutes(10),
/// );
/// let router = RoutingActionAuditor::new(std::sync::Weak::<NoRouter>::new(), Arc::new(default));
/// assert_eq!(router.name(), "rule-action-auditor");
/// # struct NoRouter;
/// # #[async_trait::async_trait]
/// # impl harness::action_auditor::OutboundRouter for NoRouter {
/// #     fn handles(&self, _: &str) -> bool { false }
/// #     async fn review(&self, _: &harness::action_auditor::ActionReview, _: &harness::action_auditor::ActionContext<'_>) -> Result<harness::action_auditor::ActionVerdict, harness::action_auditor::ActionAuditError> { unreachable!() }
/// #     async fn on_quarantine(&self, _: &harness::action_auditor::ActionReview, _: &[harness::auditor::Reason]) -> Result<(), harness::action_auditor::ActionAuditError> { Ok(()) }
/// # }
/// ```
pub struct RoutingActionAuditor {
    router: Weak<dyn OutboundRouter>,
    default: Arc<dyn ActionAuditor>,
}

impl RoutingActionAuditor {
    /// Route through `router`, falling back to `default`.
    pub fn new<R: OutboundRouter + 'static>(
        router: Weak<R>,
        default: Arc<dyn ActionAuditor>,
    ) -> Self {
        let router: Weak<dyn OutboundRouter> = router;
        Self { router, default }
    }
}

#[async_trait]
impl ActionAuditor for RoutingActionAuditor {
    fn name(&self) -> &str {
        self.default.name()
    }

    async fn review_action(
        &self,
        review: &ActionReview,
        context: &ActionContext<'_>,
    ) -> Result<ActionVerdict, ActionAuditError> {
        if !is_message_tool(&review.tool) {
            return self.default.review_action(review, context).await;
        }
        let Some(router) = self.router.upgrade() else {
            return Ok(refuse("message router unavailable; message tool refused"));
        };
        if !router.handles(&review.tool) {
            return Ok(refuse("message router does not handle this message tool"));
        }
        let verdict = router.review(review, context).await?;
        match verdict {
            ActionVerdict::Escalate { mut reasons } => {
                if let Err(error) = router.on_quarantine(review, &reasons).await {
                    tracing::error!(tool = %review.tool, %error, "quarantine record not written");
                    reasons.push(Reason::new(
                        ReasonCode::Other,
                        "quarantine record not written",
                    ));
                }
                Ok(ActionVerdict::Escalate { reasons })
            }
            other => Ok(other),
        }
    }
}

fn refuse(detail: &str) -> ActionVerdict {
    ActionVerdict::block(vec![Reason::new(ReasonCode::Other, detail)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action_auditor::rules::tests::review as sample_review;
    use crate::action_auditor::{ActionAuditLog, ActionGate};
    use crate::auditor::VerdictKind;
    use crate::effects::EffectClass;
    use crate::leases::LeaseContext;
    use std::sync::Mutex;

    fn ctx() -> ActionContext<'static> {
        ActionContext {
            max_effect: EffectClass::Repository,
            window: None,
            lease: LeaseContext::default(),
            now: chrono::Utc::now(),
            task: "",
        }
    }

    struct Fixed(&'static str, Option<ActionVerdict>);

    #[async_trait]
    impl ActionAuditor for Fixed {
        fn name(&self) -> &str {
            self.0
        }
        async fn review_action(
            &self,
            _review: &ActionReview,
            _context: &ActionContext<'_>,
        ) -> Result<ActionVerdict, ActionAuditError> {
            match &self.1 {
                Some(v) => Ok(v.clone()),
                None => Err(ActionAuditError::Timeout {
                    timeout: std::time::Duration::from_secs(1),
                }),
            }
        }
    }

    struct Hub {
        verdict: Option<ActionVerdict>,
        hook_fails: bool,
        reviewed: Mutex<Vec<String>>,
        quarantined: Mutex<Vec<(String, usize)>>,
    }

    impl Hub {
        fn new(verdict: Option<ActionVerdict>) -> Arc<Self> {
            Arc::new(Self {
                verdict,
                hook_fails: false,
                reviewed: Mutex::default(),
                quarantined: Mutex::default(),
            })
        }
    }

    #[async_trait]
    impl OutboundRouter for Hub {
        fn handles(&self, tool: &str) -> bool {
            is_message_tool(tool)
        }
        async fn review(
            &self,
            review: &ActionReview,
            _context: &ActionContext<'_>,
        ) -> Result<ActionVerdict, ActionAuditError> {
            self.reviewed.lock().unwrap().push(review.tool.clone());
            self.verdict.clone().ok_or(ActionAuditError::Timeout {
                timeout: std::time::Duration::from_secs(2),
            })
        }
        async fn on_quarantine(
            &self,
            review: &ActionReview,
            reasons: &[Reason],
        ) -> Result<(), ActionAuditError> {
            self.quarantined
                .lock()
                .unwrap()
                .push((review.tool.clone(), reasons.len()));
            if self.hook_fails {
                Err(ActionAuditError::Timeout {
                    timeout: std::time::Duration::from_secs(3),
                })
            } else {
                Ok(())
            }
        }
    }

    fn routing(hub: &Arc<Hub>, default: Fixed) -> RoutingActionAuditor {
        RoutingActionAuditor::new(Arc::downgrade(hub), Arc::new(default))
    }

    fn reason() -> Vec<Reason> {
        vec![Reason::new(ReasonCode::Other, "r")]
    }

    #[test]
    fn the_message_tool_list_is_pinned() {
        assert_eq!(
            MESSAGE_TOOLS,
            ["msg_send", "session_set_note", "sessions_query"]
        );
        for t in MESSAGE_TOOLS {
            assert!(is_message_tool(t));
        }
        for t in [
            "msg_recv",
            "run_command",
            "write_file",
            "",
            "MSG_SEND",
            "msg_send ",
        ] {
            assert!(!is_message_tool(t), "{t:?}");
        }
    }

    #[tokio::test]
    async fn non_message_tools_reach_the_default_unchanged() {
        let hub = Hub::new(Some(ActionVerdict::Allow));
        for verdict in [
            ActionVerdict::Allow,
            ActionVerdict::block(reason()),
            ActionVerdict::escalate(reason()),
        ] {
            let r = routing(&hub, Fixed("default", Some(verdict.clone())));
            let got = r
                .review_action(&sample_review("push", EffectClass::Repository), &ctx())
                .await
                .unwrap();
            assert_eq!(got, verdict);
        }
        assert!(hub.reviewed.lock().unwrap().is_empty());
        assert!(hub.quarantined.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_default_error_propagates_for_non_message_tools() {
        let hub = Hub::new(Some(ActionVerdict::Allow));
        let r = routing(&hub, Fixed("default", None));
        assert!(r
            .review_action(&sample_review("push", EffectClass::Repository), &ctx())
            .await
            .is_err());
    }

    #[tokio::test]
    async fn message_tools_go_to_the_router_not_the_default() {
        let hub = Hub::new(Some(ActionVerdict::Allow));
        let r = routing(&hub, Fixed("default", Some(ActionVerdict::block(reason()))));
        for tool in MESSAGE_TOOLS {
            let got = r
                .review_action(&sample_review(tool, EffectClass::None), &ctx())
                .await
                .unwrap();
            assert_eq!(got, ActionVerdict::Allow, "{tool}");
        }
        assert_eq!(hub.reviewed.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn a_router_block_does_not_quarantine() {
        let hub = Hub::new(Some(ActionVerdict::block(reason())));
        let r = routing(&hub, Fixed("default", Some(ActionVerdict::Allow)));
        let got = r
            .review_action(&sample_review("msg_send", EffectClass::None), &ctx())
            .await
            .unwrap();
        assert_eq!(got.kind(), VerdictKind::Block);
        assert!(hub.quarantined.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_router_allow_does_not_quarantine() {
        let hub = Hub::new(Some(ActionVerdict::Allow));
        let r = routing(&hub, Fixed("default", None));
        r.review_action(&sample_review("msg_send", EffectClass::None), &ctx())
            .await
            .unwrap();
        assert!(hub.quarantined.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_router_escalation_runs_the_hook_once_with_its_reasons() {
        let hub = Hub::new(Some(ActionVerdict::escalate(reason())));
        let r = routing(&hub, Fixed("default", Some(ActionVerdict::Allow)));
        let got = r
            .review_action(&sample_review("msg_send", EffectClass::None), &ctx())
            .await
            .unwrap();
        assert_eq!(got, ActionVerdict::escalate(reason()));
        assert!(!got
            .reasons()
            .iter()
            .any(|r| r.detail.contains("quarantine record")));
        assert_eq!(
            *hub.quarantined.lock().unwrap(),
            vec![("msg_send".to_string(), 1)]
        );
    }

    #[tokio::test]
    async fn a_failed_hook_still_escalates() {
        let hub = Arc::new(Hub {
            verdict: Some(ActionVerdict::escalate(reason())),
            hook_fails: true,
            reviewed: Mutex::default(),
            quarantined: Mutex::default(),
        });
        let r = routing(&hub, Fixed("default", Some(ActionVerdict::Allow)));
        let got = r
            .review_action(&sample_review("msg_send", EffectClass::None), &ctx())
            .await
            .unwrap();
        assert_eq!(got.kind(), VerdictKind::Escalate);
        assert_eq!(hub.quarantined.lock().unwrap().len(), 1);
        assert!(got
            .reasons()
            .iter()
            .any(|r| r.detail == "quarantine record not written"));
        assert_eq!(got.reasons().len(), reason().len() + 1);
    }

    #[tokio::test]
    async fn a_router_error_is_an_error_and_never_runs_the_hook() {
        let hub = Hub::new(None);
        let r = routing(&hub, Fixed("default", Some(ActionVerdict::Allow)));
        assert!(r
            .review_action(&sample_review("msg_send", EffectClass::None), &ctx())
            .await
            .is_err());
        assert!(hub.quarantined.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_dropped_router_blocks_message_tools_and_spares_the_rest() {
        let hub = Hub::new(Some(ActionVerdict::Allow));
        let r = routing(&hub, Fixed("default", Some(ActionVerdict::Allow)));
        drop(hub);
        for tool in MESSAGE_TOOLS {
            let got = r
                .review_action(&sample_review(tool, EffectClass::None), &ctx())
                .await
                .unwrap();
            assert_eq!(got.kind(), VerdictKind::Block, "{tool}");
        }
        let got = r
            .review_action(&sample_review("push", EffectClass::Repository), &ctx())
            .await
            .unwrap();
        assert_eq!(got, ActionVerdict::Allow);
    }

    struct Scripted {
        handles: bool,
        verdict: ActionVerdict,
        reviewed: Mutex<usize>,
    }

    impl Scripted {
        fn new(handles: bool, verdict: ActionVerdict) -> Arc<Self> {
            Arc::new(Self {
                handles,
                verdict,
                reviewed: Mutex::new(0),
            })
        }
    }

    #[async_trait]
    impl OutboundRouter for Scripted {
        fn handles(&self, _tool: &str) -> bool {
            self.handles
        }
        async fn review(
            &self,
            _: &ActionReview,
            _: &ActionContext<'_>,
        ) -> Result<ActionVerdict, ActionAuditError> {
            *self.reviewed.lock().unwrap() += 1;
            Ok(self.verdict.clone())
        }
        async fn on_quarantine(
            &self,
            _: &ActionReview,
            _: &[Reason],
        ) -> Result<(), ActionAuditError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_router_that_declines_a_message_tool_blocks_it_and_never_uses_the_default() {
        let router = Scripted::new(false, ActionVerdict::Allow);
        let r = RoutingActionAuditor::new(
            Arc::downgrade(&router),
            Arc::new(Fixed("allow-all", Some(ActionVerdict::Allow))),
        );
        for tool in MESSAGE_TOOLS {
            let got = r
                .review_action(&sample_review(tool, EffectClass::None), &ctx())
                .await
                .unwrap();
            assert_eq!(got.kind(), VerdictKind::Block, "{tool}");
        }
        assert_eq!(*router.reviewed.lock().unwrap(), 0);
    }

    #[tokio::test]
    async fn a_router_claiming_a_non_message_tool_never_sees_it() {
        let router = Scripted::new(true, ActionVerdict::block(reason()));
        let r = RoutingActionAuditor::new(
            Arc::downgrade(&router),
            Arc::new(Fixed("allow-all", Some(ActionVerdict::Allow))),
        );
        for tool in [
            "write_file",
            "push",
            "run_command",
            "msg_recv",
            "sessions_list",
        ] {
            let got = r
                .review_action(&sample_review(tool, EffectClass::Repository), &ctx())
                .await
                .unwrap();
            assert_eq!(got, ActionVerdict::Allow, "{tool}");
        }
        assert_eq!(*router.reviewed.lock().unwrap(), 0);
    }

    fn rules_default() -> Arc<dyn ActionAuditor> {
        Arc::new(crate::action_auditor::RuleActionAuditor::new(
            Arc::new(crate::windows::WindowSet::default()),
            Arc::new(crate::leases::InMemoryLeaseStore::default()),
            chrono::Duration::minutes(10),
        ))
    }

    #[tokio::test]
    async fn the_rule_auditor_alone_would_allow_a_message_tool_so_routing_must_not_fall_back() {
        let gate = ActionGate::new(rules_default(), ActionAuditLog::in_memory());
        let review = sample_review("msg_send", EffectClass::None);
        assert!(gate.run_gate(&review, &ctx()).await.is_allow());
    }

    #[tokio::test]
    async fn through_the_real_gate_a_message_tool_is_never_allowed_without_a_content_review() {
        let declining = Scripted::new(false, ActionVerdict::Allow);
        let dropped = Scripted::new(true, ActionVerdict::Allow);
        let dropped_weak = Arc::downgrade(&dropped);
        drop(dropped);
        let cases = [
            RoutingActionAuditor::new(Arc::downgrade(&declining), rules_default()),
            RoutingActionAuditor::new(dropped_weak, rules_default()),
        ];
        for routing in cases {
            let gate = ActionGate::new(Arc::new(routing), ActionAuditLog::in_memory());
            for tool in MESSAGE_TOOLS {
                let v = gate
                    .run_gate(&sample_review(tool, EffectClass::None), &ctx())
                    .await;
                assert_eq!(v.kind(), VerdictKind::Block, "{tool}");
            }
        }
    }

    #[test]
    fn the_name_is_the_default_auditors() {
        let hub = Hub::new(None);
        assert_eq!(routing(&hub, Fixed("default", None)).name(), "default");
    }

    #[tokio::test]
    async fn through_the_gate_a_router_error_is_a_block() {
        let hub = Hub::new(None);
        let r = routing(&hub, Fixed("default", Some(ActionVerdict::Allow)));
        let gate = ActionGate::new(Arc::new(r), ActionAuditLog::in_memory());
        let v = gate
            .run_gate(&sample_review("msg_send", EffectClass::None), &ctx())
            .await;
        assert_eq!(v.kind(), VerdictKind::Block);
    }
}
