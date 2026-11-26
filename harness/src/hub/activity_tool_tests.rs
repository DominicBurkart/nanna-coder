use super::activity_sink::tests::FailingStore;
use crate::action_auditor::{IdentifierOracle, NullOracle, SharedOracle};
use crate::effects::EffectClass;
use crate::escalation::{Escalation, EscalationLog, EscalationSource, Severity};
use crate::hub::activity_sink::ActivitySink;
use crate::hub::lineage::SessionId;
use crate::hub::store::{
    ActivityEntry, ActivityStore, HubLock, InMemoryActivityStore, JsonlActivityStore,
};
use crate::leases::SystemClock;
use crate::task::TaskId;
use crate::tools::{Tool, ToolError, ToolRegistry, ToolResult};
use async_trait::async_trait;
use chrono::Utc;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

const PLANTED_KEY: &str = "Zx9Qw3Lm7Rt5Yp2Vb8Nc4";
const PLANTED_VALUE: &str = "plain-value-Qw8Er5Ty2Ui9";
const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

fn planted_token() -> String {
    format!("{}_{}", "ghp", "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8")
}

struct StubTool {
    name: String,
    keys: Vec<String>,
    class: EffectClass,
    fail: bool,
}

impl StubTool {
    fn boxed(name: &str, keys: &[&str], class: EffectClass, fail: bool) -> Box<dyn Tool> {
        let keys = keys.iter().map(|key| key.to_string()).collect();
        let name = name.to_string();
        Box::new(Self {
            name,
            keys,
            class,
            fail,
        })
    }
}

#[async_trait]
impl Tool for StubTool {
    fn definition(&self) -> ToolDefinition {
        let property = || PropertySchema {
            schema_type: SchemaType::String,
            description: None,
            items: None,
        };
        let properties: HashMap<String, PropertySchema> = self
            .keys
            .iter()
            .map(|key| (key.clone(), property()))
            .collect();
        let properties = (!properties.is_empty()).then_some(properties);
        let schema = JsonSchema {
            schema_type: SchemaType::Object,
            properties,
            required: None,
        };
        ToolDefinition {
            function: FunctionDefinition {
                name: self.name.clone(),
                description: String::new(),
                parameters: schema,
            },
        }
    }

    async fn execute(&self, _args: Value) -> ToolResult<Value> {
        let message = "refused by the test tool".to_string();
        let refused: ToolResult<Value> = Err(ToolError::InvalidArguments { message });
        let done: ToolResult<Value> = Ok(json!({ "done": self.name }));
        match self.fail {
            true => refused,
            false => done,
        }
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn effect_class(&self) -> EffectClass {
        self.class
    }
}

struct Vouches;

impl IdentifierOracle for Vouches {
    fn is_known_git_object(&self, hex: &str) -> bool {
        hex == SHA
    }

    fn is_known_hub_id(&self, _id: &str) -> bool {
        false
    }
}

fn session() -> SessionId {
    SessionId::from_task(TaskId("sink-session".to_string()))
}

fn sink_over(store: Arc<dyn ActivityStore>, oracle: SharedOracle) -> Arc<ActivitySink> {
    let clock = Arc::new(SystemClock);
    Arc::new(ActivitySink::new(store, session(), oracle, clock))
}

fn null() -> SharedOracle {
    Arc::new(NullOracle)
}

fn registry_with(sink: Option<Arc<ActivitySink>>) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.register(StubTool::boxed(
        "alpha_tool",
        &["alpha", "path"],
        EffectClass::None,
        false,
    ));
    registry.register(StubTool::boxed(
        "beta_tool",
        &["beta"],
        EffectClass::None,
        false,
    ));
    registry.register(StubTool::boxed(
        "failing_tool",
        &["path"],
        EffectClass::None,
        true,
    ));
    registry.register(StubTool::boxed(
        "push_tool",
        &["path"],
        EffectClass::Repository,
        false,
    ));
    registry.register(StubTool::boxed("bare_tool", &[], EffectClass::None, false));
    match sink {
        Some(sink) => registry.with_activity_sink(sink),
        None => registry,
    }
}

async fn run(registry: &ToolRegistry, name: &str, args: Value) -> ToolResult<Value> {
    registry.execute(name, args).await
}

fn entries(store: &dyn ActivityStore) -> Vec<ActivityEntry> {
    store.for_session(session(), usize::MAX).unwrap()
}

fn call_of(entry: &ActivityEntry) -> (String, String, bool) {
    let value = serde_json::to_value(entry).unwrap();
    let tool = value["tool"].as_str().unwrap().to_string();
    let args = value["args_summary"].as_str().unwrap().to_string();
    (tool, args, value["ok"].as_bool().unwrap())
}

fn memory() -> (Arc<InMemoryActivityStore>, ToolRegistry) {
    let store = Arc::new(InMemoryActivityStore::new());
    let registry = registry_with(Some(sink_over(store.clone(), null())));
    (store, registry)
}

#[tokio::test]
async fn a_registered_tool_call_is_logged_with_its_registered_name_and_its_outcome() {
    let (store, registry) = memory();
    run(&registry, "alpha_tool", json!({"path": "a/b"}))
        .await
        .unwrap();
    run(&registry, "failing_tool", json!({"path": "a"}))
        .await
        .unwrap_err();
    let logged: Vec<_> = entries(store.as_ref()).iter().map(call_of).collect();
    let expected = [
        ("failing_tool".to_string(), "path=str:1".to_string(), false),
        ("alpha_tool".to_string(), "path=str:3".to_string(), true),
    ];
    assert_eq!(logged, expected);
}

#[tokio::test]
async fn a_gate_denied_call_is_logged_as_not_ok() {
    let (store, registry) = memory();
    let err = run(&registry, "push_tool", json!({"path": "x"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::ActionDenied(_)));
    let logged: Vec<_> = entries(store.as_ref()).iter().map(call_of).collect();
    assert_eq!(
        logged,
        [("push_tool".to_string(), "path=str:1".to_string(), false)]
    );
}

#[tokio::test]
async fn a_call_stopped_by_the_production_hold_is_logged_as_not_ok() {
    let store = Arc::new(InMemoryActivityStore::new());
    let log = Arc::new(EscalationLog::in_memory());
    let incident = Escalation::new(Severity::Incident, EscalationSource::Rollout, "r/r", "down")
        .with_id("inc-1");
    log.hold(&incident, Utc::now()).unwrap();
    let mut registry = ToolRegistry::new()
        .with_production_hold(log, None)
        .with_activity_sink(sink_over(store.clone(), null()));
    registry.register(StubTool::boxed(
        "deploy_tool",
        &["path"],
        EffectClass::Production,
        false,
    ));
    let err = run(&registry, "deploy_tool", json!({})).await.unwrap_err();
    assert!(matches!(err, ToolError::ProductionHeld { .. }));
    let logged: Vec<_> = entries(store.as_ref()).iter().map(call_of).collect();
    assert_eq!(logged, [("deploy_tool".to_string(), String::new(), false)]);
}

#[tokio::test]
async fn an_unregistered_tool_name_is_never_logged_whatever_it_is() {
    let (store, registry) = memory();
    let token = planted_token();
    for name in [token.as_str(), PLANTED_KEY, "", "alpha_tool "] {
        let err = registry.execute(name, json!({PLANTED_KEY: 1})).await;
        assert!(matches!(err, Err(ToolError::NotFound { .. })));
    }
    assert!(entries(store.as_ref()).is_empty());
}

#[tokio::test]
async fn a_scope_denied_call_is_never_logged() {
    let store = Arc::new(InMemoryActivityStore::new());
    let mut identity = crate::identity::example();
    identity.scope.tools = vec!["alpha_tool".parse().unwrap()];
    let scoped = registry_with(None).scoped_for(&identity);
    let registry = scoped.with_activity_sink(sink_over(store.clone(), null()));
    let token = planted_token();
    let denied = registry.execute(&token, json!({})).await.unwrap_err();
    assert!(matches!(denied, ToolError::ScopeDenied(_)));
    let beta = registry.execute("beta_tool", json!({})).await.unwrap_err();
    assert!(matches!(beta, ToolError::ScopeDenied(_)));
    assert!(entries(store.as_ref()).is_empty());
    registry.execute("alpha_tool", json!({})).await.unwrap();
    assert_eq!(entries(store.as_ref()).len(), 1);
}

#[tokio::test]
async fn an_undeclared_key_is_recorded_as_a_question_mark_whatever_it_looks_like() {
    let keys = [
        PLANTED_KEY.to_string(),
        planted_token(),
        "-----BEGIN PRIVATE KEY-----".to_string(),
        "dGhpcyBpcyBhIHNlY3JldCBrZXkgdmFsdWU=".to_string(),
        "k".to_string(),
        "clé-秘密".to_string(),
        "beta".to_string(),
        "a=b".to_string(),
        "has space".to_string(),
    ];
    for key in keys {
        let (store, registry) = memory();
        let args = json!({ key.as_str(): 1, "path": "a" });
        registry.execute("alpha_tool", args).await.unwrap();
        let (_, summary, _) = call_of(&entries(store.as_ref())[0]);
        assert_eq!(summary, "?=num path=str:1", "{key}");
    }
}

#[tokio::test]
async fn the_declared_names_are_the_called_tools_own_schema_not_every_tools() {
    let (store, registry) = memory();
    run(&registry, "beta_tool", json!({"alpha": 1, "beta": 2}))
        .await
        .unwrap();
    run(&registry, "alpha_tool", json!({"alpha": 1, "beta": 2}))
        .await
        .unwrap();
    run(&registry, "bare_tool", json!({"alpha": 1}))
        .await
        .unwrap();
    let logged: Vec<_> = entries(store.as_ref()).iter().map(call_of).collect();
    let summaries: Vec<&str> = logged.iter().map(|(_, args, _)| args.as_str()).collect();
    assert_eq!(summaries, ["?=num", "?=num alpha=num", "?=num beta=num"]);
}

#[tokio::test]
async fn values_are_never_stored_only_their_kind_and_length() {
    let (store, registry) = memory();
    let token = planted_token();
    let content = "x".repeat(4096);
    let args = json!({"path": token, "alpha": content, "zz": [1, 2, 3]});
    registry.execute("alpha_tool", args).await.unwrap();
    let (_, summary, _) = call_of(&entries(store.as_ref())[0]);
    assert_eq!(summary, "?=arr:3 alpha=str:4096 path=str:40");
    assert!(!summary.contains(&token));
}

#[tokio::test]
async fn the_session_is_the_hosts_never_one_named_in_the_arguments() {
    let (store, registry) = memory();
    let forged = json!({"session": "other-session", "session_id": "other-session"});
    registry.execute("alpha_tool", forged).await.unwrap();
    let other = SessionId::from_task(TaskId("other-session".to_string()));
    assert!(store.for_session(other, usize::MAX).unwrap().is_empty());
    assert_eq!(entries(store.as_ref()).len(), 1);
}

#[tokio::test]
async fn the_senders_oracle_decides_whether_a_declared_identifier_is_kept() {
    let mut vouched = ToolRegistry::new();
    vouched.register(StubTool::boxed(
        "sha_tool",
        &[SHA],
        EffectClass::None,
        false,
    ));
    let mut plain = ToolRegistry::new();
    plain.register(StubTool::boxed(
        "sha_tool",
        &[SHA],
        EffectClass::None,
        false,
    ));
    let known = Arc::new(InMemoryActivityStore::new());
    let unknown = Arc::new(InMemoryActivityStore::new());
    let vouched = vouched.with_activity_sink(sink_over(known.clone(), Arc::new(Vouches)));
    let plain = plain.with_activity_sink(sink_over(unknown.clone(), null()));
    vouched.execute("sha_tool", json!({SHA: 1})).await.unwrap();
    plain.execute("sha_tool", json!({SHA: 1})).await.unwrap();
    let (_, kept, _) = call_of(&entries(known.as_ref())[0]);
    let (_, withheld, _) = call_of(&entries(unknown.as_ref())[0]);
    assert_eq!(kept, format!("{SHA}=num"));
    assert_eq!(withheld, "[withheld: secret_leak]");
}

#[test]
fn a_failing_append_never_changes_the_result_and_is_counted() {
    let _serial = super::activity_sink::tests::serial();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(failing_appends_leave_results_alone());
}

async fn failing_appends_leave_results_alone() {
    let failing = FailingStore::new();
    let sink = sink_over(failing.clone(), null());
    let logged = registry_with(Some(sink.clone()));
    let bare = registry_with(None);
    let calls = [
        ("alpha_tool", json!({"path": "a"})),
        ("failing_tool", json!({})),
        ("push_tool", json!({})),
        ("missing_tool", json!({})),
    ];
    for (name, args) in calls {
        let with = logged.execute(name, args.clone()).await;
        let without = bare.execute(name, args).await;
        assert_eq!(format!("{with:?}"), format!("{without:?}"), "{name}");
    }
    assert_eq!(failing.attempts(), 3);
    assert_eq!(sink.failures(), 3);
}

#[tokio::test]
async fn a_registry_with_no_sink_returns_the_same_results_as_before() {
    let bare = registry_with(None);
    let ok = bare.execute("alpha_tool", json!({"path": "a"})).await;
    assert_eq!(ok.unwrap(), json!({"done": "alpha_tool"}));
    let failed = bare.execute("failing_tool", json!({})).await.unwrap_err();
    assert!(matches!(failed, ToolError::InvalidArguments { .. }));
    let denied = bare.execute("push_tool", json!({})).await.unwrap_err();
    assert!(matches!(denied, ToolError::ActionDenied(_)));
    let missing = bare.execute("missing_tool", json!({})).await.unwrap_err();
    assert!(matches!(missing, ToolError::NotFound { .. }));
}

#[tokio::test]
async fn nothing_planted_reaches_the_journal_the_listing_debug_or_the_log() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("activity.jsonl");
    let lock = HubLock::acquire(dir.path()).unwrap();
    let store = Arc::new(JsonlActivityStore::open(&path, &lock).unwrap());
    let registry = registry_with(Some(sink_over(store.clone(), null())));
    let token = planted_token();
    let args = json!({PLANTED_KEY: token, "path": token.clone(), "alpha": PLANTED_VALUE});
    registry.execute("alpha_tool", args.clone()).await.unwrap();
    registry.execute(&token, args).await.unwrap_err();
    let bytes = std::fs::read_to_string(&path).unwrap();
    let listing = entries(store.as_ref());
    let debug = format!("{listing:?}");
    let json = serde_json::to_string(&listing).unwrap();
    for text in [bytes, debug, json] {
        assert!(!text.contains(PLANTED_KEY), "{text}");
        assert!(!text.contains(&token), "{text}");
        assert!(!text.contains(PLANTED_VALUE), "{text}");
    }
    assert_eq!(listing.len(), 1);
}
