use crate::action_auditor::{
    ActionAuditLogEntry, ActionContext, ActionDenied, ActionGate, ActionReview, ActionVerdict,
};
use crate::auditor::{Reason, ReasonCode};
use crate::effects::EffectClass;
use crate::identity::AgentIdentity;
use crate::leases::LeaseContext;
use crate::protected::{ProtectedPathViolation, ProtectedPaths};
use crate::scope::{
    canonical_root, relative_to, resolve_path, resolve_path_guarded,
    validate_path_within_workspace, DenialReason, PathAccess, PathScope, ScopeDenial, ScopeError,
    UNSCOPED_IDENTITY,
};
use crate::task::TaskId;
use async_trait::async_trait;
use chrono::Utc;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum ToolError {
    #[error("Invalid arguments: {message}")]
    InvalidArguments { message: String },

    #[error("Execution failed: {message}")]
    ExecutionFailed { message: String },

    #[error("Tool not found: {name}")]
    NotFound { name: String },

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Path security violation: {message}")]
    PathSecurityViolation { message: String },

    #[error("Scope denial: {0}")]
    ScopeDenied(ScopeDenial),

    #[error("Protected path: {0}")]
    ProtectedPath(ProtectedPathViolation),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// The action auditor blocked or escalated an effectful tool call
    /// before it reached the tool.
    #[error("{0}")]
    ActionDenied(ActionDenied),
}

pub type ToolResult<T> = Result<T, ToolError>;

#[async_trait]
pub trait Tool: Send + Sync {
    fn definition(&self) -> ToolDefinition;
    async fn execute(&self, args: Value) -> ToolResult<Value>;
    fn name(&self) -> &str;
    /// The largest blast radius a call to this tool can reach.
    ///
    /// There is deliberately no default: every tool must state its class
    /// explicitly so that policy layers never treat an unclassified tool as
    /// harmless. Tools that can reach several classes (shell runners, for
    /// example) declare the maximum and are treated as that class.
    fn effect_class(&self) -> EffectClass;
}

/// Fixed, per-task context an effectful tool call is reviewed against: which
/// task it belongs to, the calling identity's effect ceiling, the
/// availability window (if any) `Sandbox`/`Production` calls target, and
/// enough of the deployment context to derive coordination leases (see
/// [`LeaseContext`]).
#[derive(Debug, Clone)]
pub struct ActionSubject {
    /// Task the registry's calls belong to.
    pub task_id: TaskId,
    /// Widest effect class the calling identity may reach.
    pub max_effect: EffectClass,
    /// Name of the availability window `Sandbox`/`Production` calls target.
    pub window: Option<String>,
    /// Repository in `owner/name` form.
    pub repo: String,
    /// Branch pushed to, for `Repository`-class calls.
    pub branch: Option<String>,
    /// Pull request deployed, for `Sandbox`-class calls.
    pub pr: Option<u64>,
    /// Environment rolled out to, for `Production`-class calls.
    pub environment: Option<String>,
    /// Path globs the task edits.
    pub paths: Vec<String>,
}

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
    identity: Option<String>,
    denials: Mutex<Vec<ScopeDenial>>,
    action_gate: Option<Arc<ActionGate>>,
    action_subject: Option<ActionSubject>,
    action_prior: Mutex<Vec<EffectClass>>,
    action_denial_count: Mutex<usize>,
    action_log: Mutex<Vec<ActionAuditLogEntry>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            identity: None,
            denials: Mutex::new(Vec::new()),
            action_gate: None,
            action_subject: None,
            action_prior: Mutex::new(Vec::new()),
            action_denial_count: Mutex::new(0),
            action_log: Mutex::new(Vec::new()),
        }
    }

    /// Attach an action-review gate: every subsequent call to a tool whose
    /// [`Tool::effect_class`] is at least [`EffectClass::Repository`] is
    /// reviewed through `gate` under `subject` before it runs. Without a
    /// gate, every such call is refused by default (see [`Self::execute`]),
    /// so attaching one is how a caller opts a registry in rather than how
    /// it opts one out.
    pub fn with_action_gate(mut self, gate: Arc<ActionGate>, subject: ActionSubject) -> Self {
        self.action_gate = Some(gate);
        self.action_subject = Some(subject);
        self
    }

    /// Every action review recorded so far for this registry's task, in
    /// call order.
    pub fn action_reviews(&self) -> Vec<ActionAuditLogEntry> {
        self.action_log.lock().expect("action log poisoned").clone()
    }

    /// Keep only the tools `identity` may call: those whose name matches a
    /// `scope.tools` pattern and whose effect class is at most
    /// `scope.max_effect`. Everything else is dropped, so it never appears in
    /// the definitions sent to the model. Calls to dropped or unknown tools
    /// are refused with [`ToolError::ScopeDenied`] and recorded.
    ///
    /// ```
    /// use harness::effects::EffectClass;
    /// use harness::identity::AgentIdentity;
    /// use harness::tools::create_tool_registry;
    ///
    /// let toml = r#"
    /// [identity]
    /// name = "reader"
    /// description = "Reads code."
    /// loop = "inner"
    /// model = "gemma4:e4b"
    /// system_prompt = { inline = "Read." }
    ///
    /// [scope]
    /// repos = []
    /// paths = []
    /// max_effect = "workspace"
    /// tools = ["read_file", "write_file", "github_*"]
    ///
    /// [limits]
    /// max_iterations = 10
    /// max_wall_clock_secs = 60
    /// max_concurrent = 1
    /// "#;
    /// let identity = AgentIdentity::from_toml_str(toml, "reader.toml").unwrap();
    /// let scoped = create_tool_registry(std::path::Path::new(".")).scoped_for(&identity);
    ///
    /// let mut names = scoped.list_tools();
    /// names.sort_unstable();
    /// assert_eq!(names, vec!["read_file", "write_file"]);
    /// assert_eq!(scoped.identity(), Some("reader"));
    /// assert!(scoped.get_tool("github_pr_status").is_none(), "repository class exceeds the ceiling");
    /// assert!(scoped.get_tool("search").is_none(), "not named in scope.tools");
    /// assert_eq!(scoped.denial_count(), 0);
    /// ```
    pub fn scoped_for(mut self, identity: &AgentIdentity) -> Self {
        let keep = |name: &String, tool: &mut Box<dyn Tool>| {
            identity.allows_tool(name) && identity.allows_effect(tool.effect_class())
        };
        self.tools.retain(keep);
        self.identity = Some(identity.name().to_string());
        self
    }

    /// Name of the identity this registry is scoped to, if any.
    pub fn identity(&self) -> Option<&str> {
        self.identity.as_deref()
    }

    /// Every call refused so far, in order.
    pub fn denials(&self) -> Vec<ScopeDenial> {
        self.denials.lock().expect("denial log poisoned").clone()
    }

    /// Number of refused calls, for escalation on repeats.
    pub fn denial_count(&self) -> usize {
        self.denials.lock().expect("denial log poisoned").len()
    }

    fn record(&self, denial: ScopeDenial) {
        let mut log = self.denials.lock().expect("denial log poisoned");
        log.push(denial);
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        let name = tool.name().to_string();
        self.tools.insert(name, tool);
    }

    pub fn get_tool(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    pub fn list_tools(&self) -> Vec<&str> {
        self.tools.keys().map(|s| s.as_str()).collect()
    }

    pub fn get_definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|tool| tool.definition()).collect()
    }

    /// Dispatch `name(args)`. A tool whose [`Tool::effect_class`] is at
    /// least [`EffectClass::Repository`] is reviewed by the action auditor
    /// first: this is the only chokepoint tool calls pass through, so the
    /// review is structural (every effectful call goes through it) rather
    /// than something each `Tool` implementation must remember to do. A
    /// call below that threshold reaches the tool with no review at all,
    /// matching the epic's own design principle that the container-isolated
    /// inner loop needs no gate.
    pub async fn execute(&self, name: &str, args: Value) -> ToolResult<Value> {
        if let Some(class) = self.tools.get(name).map(|tool| tool.effect_class()) {
            if class >= EffectClass::Repository {
                if let Err(denied) = self.review_action(name, &args, class).await {
                    return Err(ToolError::ActionDenied(denied));
                }
            }
        }
        let outcome = match (self.tools.get(name), &self.identity) {
            (Some(tool), _) => tool.execute(args).await,
            (None, Some(identity)) => Err(ToolError::ScopeDenied(ScopeDenial {
                identity: identity.clone(),
                tool: name.to_string(),
                reason: DenialReason::ToolNotInScope,
            })),
            (None, None) => Err(ToolError::NotFound {
                name: name.to_string(),
            }),
        };
        match &outcome {
            Err(ToolError::ScopeDenied(denial)) => self.record(denial.clone()),
            Err(ToolError::ProtectedPath(violation)) => {
                let mut denial = ScopeDenial::protected(name, violation);
                if let Some(identity) = &self.identity {
                    denial.identity = identity.clone();
                }
                self.record(denial);
            }
            _ => {}
        }
        outcome
    }

    /// Review one effectful call: consult [`Self::action_gate`] under
    /// [`Self::action_subject`] when both are configured, else refuse by
    /// default (fail closed, never silently allow an unreviewed effectful
    /// action). Every review, allowed or not, is appended to
    /// [`Self::action_reviews`]. The third denial in this registry's task
    /// is upgraded to [`ActionDenied::Escalate`] regardless of what the
    /// auditor itself returned.
    async fn review_action(
        &self,
        name: &str,
        args: &Value,
        class: EffectClass,
    ) -> Result<(), ActionDenied> {
        let identity = self
            .identity
            .clone()
            .unwrap_or_else(|| UNSCOPED_IDENTITY.to_string());
        let task_id = self
            .action_subject
            .as_ref()
            .map(|subject| subject.task_id.clone())
            .unwrap_or_else(|| TaskId(UNSCOPED_IDENTITY.to_string()));
        let review = ActionReview {
            identity,
            task_id,
            tool: name.to_string(),
            args: args.clone(),
            effect_class: class,
            prior_actions: self
                .action_prior
                .lock()
                .expect("action prior poisoned")
                .clone(),
        };
        let verdict = match (&self.action_gate, &self.action_subject) {
            (Some(gate), Some(subject)) => {
                let ctx = ActionContext {
                    max_effect: subject.max_effect,
                    window: subject.window.as_deref(),
                    lease: LeaseContext {
                        repo: &subject.repo,
                        branch: subject.branch.as_deref(),
                        pr: subject.pr,
                        environment: subject.environment.as_deref(),
                        paths: &subject.paths,
                    },
                    now: Utc::now(),
                };
                gate.run_gate(&review, &ctx).await
            }
            _ => {
                let reason = Reason::new(
                    ReasonCode::Other,
                    "no action auditor configured for this registry; effectful tool calls are refused by default",
                );
                ActionVerdict::block(vec![reason])
            }
        };
        let verdict = self.apply_denial_policy(verdict);
        self.action_log
            .lock()
            .expect("action log poisoned")
            .push(ActionAuditLogEntry {
                review,
                verdict: verdict.clone(),
            });
        match verdict {
            ActionVerdict::Allow => {
                self.action_prior
                    .lock()
                    .expect("action prior poisoned")
                    .push(class);
                Ok(())
            }
            ActionVerdict::Block { reasons } => Err(ActionDenied::Block { reasons }),
            ActionVerdict::Escalate { reasons } => Err(ActionDenied::Escalate { reasons }),
        }
    }

    /// Count a `Block`/`Escalate` verdict against this task and upgrade the
    /// third one to `Escalate` (a `RepeatedDenials` reason is appended so
    /// the upgrade is visible in [`Self::action_reviews`], not just in the
    /// [`ActionDenied`] returned to the caller).
    fn apply_denial_policy(&self, verdict: ActionVerdict) -> ActionVerdict {
        match verdict {
            ActionVerdict::Allow => ActionVerdict::Allow,
            ActionVerdict::Block { mut reasons } => {
                let mut count = self
                    .action_denial_count
                    .lock()
                    .expect("action denial count poisoned");
                *count += 1;
                if *count < 3 {
                    return ActionVerdict::Block { reasons };
                }
                let detail = format!(
                    "{} denials recorded for this task; halting for review",
                    *count
                );
                reasons.push(Reason::new(ReasonCode::RepeatedDenials, detail));
                ActionVerdict::escalate(reasons)
            }
            ActionVerdict::Escalate { reasons } => {
                *self
                    .action_denial_count
                    .lock()
                    .expect("action denial count poisoned") += 1;
                ActionVerdict::Escalate { reasons }
            }
        }
    }

    /// Effect class declared by the named tool, or `None` when no such tool
    /// is registered.
    ///
    /// ```
    /// use harness::effects::EffectClass;
    /// use harness::tools::create_tool_registry;
    ///
    /// let registry = create_tool_registry(std::path::Path::new("."));
    /// assert_eq!(registry.effect_class_of("read_file"), Some(EffectClass::None));
    /// assert_eq!(registry.effect_class_of("write_file"), Some(EffectClass::Workspace));
    /// assert_eq!(registry.effect_class_of("no_such_tool"), None);
    /// ```
    pub fn effect_class_of(&self, name: &str) -> Option<EffectClass> {
        self.tools.get(name).map(|tool| tool.effect_class())
    }

    fn tools_where(&self, keep: &dyn Fn(EffectClass) -> bool) -> Vec<&dyn Tool> {
        let candidates = self.tools.values().map(|tool| tool.as_ref());
        let mut selected: Vec<_> = candidates.filter(|t| keep(t.effect_class())).collect();
        selected.sort_by(|a, b| a.name().cmp(b.name()));
        selected
    }

    /// Every tool whose effect class is at most `ceiling`, sorted by name.
    ///
    /// ```
    /// use harness::effects::EffectClass;
    /// use harness::tools::create_tool_registry;
    ///
    /// let registry = create_tool_registry(std::path::Path::new("."));
    /// let allowed: Vec<&str> = registry
    ///     .at_most(EffectClass::Workspace)
    ///     .iter()
    ///     .map(|tool| tool.name())
    ///     .collect();
    /// assert!(allowed.contains(&"read_file"));
    /// assert!(allowed.contains(&"write_file"));
    /// assert!(!allowed.contains(&"github_pr_status"));
    ///
    /// let read_only = registry.at_most(EffectClass::None);
    /// assert!(read_only.iter().all(|tool| tool.effect_class() == EffectClass::None));
    /// ```
    pub fn at_most(&self, ceiling: EffectClass) -> Vec<&dyn Tool> {
        self.tools_where(&|class| class <= ceiling)
    }

    /// Every tool declaring exactly `class`, sorted by name.
    ///
    /// ```
    /// use harness::effects::EffectClass;
    /// use harness::tools::create_tool_registry;
    ///
    /// let registry = create_tool_registry(std::path::Path::new("."));
    /// let names: Vec<&str> = registry
    ///     .with_class(EffectClass::Repository)
    ///     .iter()
    ///     .map(|tool| tool.name())
    ///     .collect();
    /// assert_eq!(names, vec!["git_push_branch", "github_pr_status"]);
    /// assert!(registry.with_class(EffectClass::Production).is_empty());
    /// ```
    pub fn with_class(&self, class: EffectClass) -> Vec<&dyn Tool> {
        self.tools_where(&|candidate| candidate == class)
    }

    /// Tool names grouped by effect class. Every class is present as a key,
    /// with an empty list for classes no registered tool declares.
    ///
    /// ```
    /// use harness::effects::EffectClass;
    /// use harness::tools::create_tool_registry;
    ///
    /// let registry = create_tool_registry(std::path::Path::new("."));
    /// let grouped = registry.by_class();
    /// assert_eq!(grouped.len(), EffectClass::ALL.len());
    /// assert_eq!(grouped[&EffectClass::Workspace], vec!["write_file"]);
    /// assert!(grouped[&EffectClass::Ci].is_empty());
    /// ```
    pub fn by_class(&self) -> BTreeMap<EffectClass, Vec<&str>> {
        EffectClass::ALL
            .into_iter()
            .map(|class| {
                let names = self
                    .with_class(class)
                    .into_iter()
                    .map(|tool| tool.name())
                    .collect();
                (class, names)
            })
            .collect()
    }

    /// Drop every tool whose effect class exceeds `ceiling`, leaving a
    /// registry that can only produce effects at or below that class.
    ///
    /// ```
    /// use harness::effects::EffectClass;
    /// use harness::tools::create_tool_registry;
    ///
    /// let mut registry = create_tool_registry(std::path::Path::new("."));
    /// registry.retain_at_most(EffectClass::None);
    /// assert!(registry.get_tool("read_file").is_some());
    /// assert!(registry.get_tool("write_file").is_none());
    /// assert!(registry.get_tool("github_pr_status").is_none());
    /// ```
    pub fn retain_at_most(&mut self, ceiling: EffectClass) {
        self.tools.retain(|_, tool| tool.effect_class() <= ceiling);
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub struct EchoTool;

impl EchoTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for EchoTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for EchoTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "echo".to_string(),
                description: "Echo back the provided message".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "message".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some("The message to echo back".to_string()),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["message".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "Missing or invalid 'message' parameter".to_string(),
            })?;

        Ok(json!({
            "echoed": message,
            "timestamp": chrono::Utc::now().to_rfc3339()
        }))
    }

    fn name(&self) -> &str {
        "echo"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::None
    }
}

pub struct CalculatorTool;

impl CalculatorTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for CalculatorTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for CalculatorTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "calculate".to_string(),
                description: "Perform basic arithmetic calculations".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "operation".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "The operation: add, subtract, multiply, divide".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "a".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Number,
                                description: Some("First number".to_string()),
                                items: None,
                            },
                        );
                        props.insert(
                            "b".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Number,
                                description: Some("Second number".to_string()),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec![
                        "operation".to_string(),
                        "a".to_string(),
                        "b".to_string(),
                    ]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let operation = args
            .get("operation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "Missing or invalid 'operation' parameter".to_string(),
            })?;

        let a =
            args.get("a")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| ToolError::InvalidArguments {
                    message: "Missing or invalid 'a' parameter".to_string(),
                })?;

        let b =
            args.get("b")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| ToolError::InvalidArguments {
                    message: "Missing or invalid 'b' parameter".to_string(),
                })?;

        let result = match operation {
            "add" => a + b,
            "subtract" => a - b,
            "multiply" => a * b,
            "divide" => {
                if b == 0.0 {
                    return Err(ToolError::ExecutionFailed {
                        message: "Division by zero".to_string(),
                    });
                }
                a / b
            }
            _ => {
                return Err(ToolError::InvalidArguments {
                    message: format!("Unknown operation: {}", operation),
                });
            }
        };

        Ok(json!({
            "operation": operation,
            "operands": [a, b],
            "result": result
        }))
    }

    fn name(&self) -> &str {
        "calculate"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::None
    }
}

pub struct ReadFileTool {
    workspace_root: PathBuf,
    scope: Option<PathScope>,
}

impl ReadFileTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self::scoped(workspace_root, None)
    }

    /// A reader that, when `scope` is present, refuses files outside the
    /// identity's `scope.read_paths`.
    pub fn scoped(workspace_root: PathBuf, scope: Option<PathScope>) -> Self {
        Self {
            workspace_root,
            scope,
        }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "read_file".to_string(),
                description:
                    "Read the contents of a file. Returns the file content with line numbers."
                        .to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "path".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Path to the file (relative to workspace root)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "start_line".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Integer,
                                description: Some(
                                    "Starting line number (1-indexed, optional)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "end_line".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Integer,
                                description: Some(
                                    "Ending line number (inclusive, optional)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["path".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let path_str = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
            ToolError::InvalidArguments {
                message: "Missing or invalid 'path' parameter".to_string(),
            }
        })?;

        let path = Path::new(path_str);
        let scope = self.scope.as_ref();
        let root = &self.workspace_root;
        let safe_path = resolve_path(scope, "read_file", PathAccess::Read, path, root)?;

        let content = std::fs::read_to_string(&safe_path)?;
        let lines: Vec<&str> = content.lines().collect();

        let start = args
            .get("start_line")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).saturating_sub(1))
            .unwrap_or(0);

        let end = args
            .get("end_line")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(lines.len());

        let selected_lines: Vec<String> = lines
            .iter()
            .enumerate()
            .skip(start)
            .take(end.saturating_sub(start))
            .map(|(i, line)| format!("{:>6}  {}", i + 1, line))
            .collect();

        Ok(json!({
            "path": path_str,
            "content": selected_lines.join("\n"),
            "total_lines": lines.len(),
            "lines_shown": selected_lines.len()
        }))
    }

    fn name(&self) -> &str {
        "read_file"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::None
    }
}

pub struct WriteFileTool {
    workspace_root: PathBuf,
    scope: Option<PathScope>,
    protected: ProtectedPaths,
}

impl WriteFileTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self::scoped(workspace_root, None)
    }

    /// A writer that, when `scope` is present, refuses paths outside the
    /// identity's `scope.paths`, and always refuses
    /// [`ProtectedPaths::for_repo`] of the workspace.
    pub fn scoped(workspace_root: PathBuf, scope: Option<PathScope>) -> Self {
        let protected = ProtectedPaths::for_repo(&workspace_root);
        Self::guarded(workspace_root, scope, protected)
    }

    /// [`WriteFileTool::scoped`] with an explicit protected set.
    pub fn guarded(
        workspace_root: PathBuf,
        scope: Option<PathScope>,
        protected: ProtectedPaths,
    ) -> Self {
        Self {
            workspace_root,
            scope,
            protected,
        }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "write_file".to_string(),
                description: "Write content to a file. Creates the file if it doesn't exist, overwrites if it does.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "path".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some("Path to the file (relative to workspace root)".to_string()),
                                items: None,
                            },
                        );
                        props.insert(
                            "content".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some("Content to write to the file".to_string()),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["path".to_string(), "content".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let path_str = args.get("path").and_then(|v| v.as_str()).ok_or_else(|| {
            ToolError::InvalidArguments {
                message: "Missing or invalid 'path' parameter".to_string(),
            }
        })?;

        let content = args
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "Missing or invalid 'content' parameter".to_string(),
            })?;

        let path = Path::new(path_str);
        let scope = self.scope.as_ref();
        let root = &self.workspace_root;
        let access = PathAccess::Write;
        let protected = &self.protected;
        let safe_path = resolve_path_guarded(scope, protected, "write_file", access, path, root)?;

        if let Some(parent) = safe_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(&safe_path, content)?;

        Ok(json!({
            "path": path_str,
            "bytes_written": content.len(),
            "success": true
        }))
    }

    fn name(&self) -> &str {
        "write_file"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Workspace
    }
}

pub struct ListDirTool {
    workspace_root: PathBuf,
    scope: Option<PathScope>,
}

impl ListDirTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self::scoped(workspace_root, None)
    }

    /// A lister that, when `scope` is present, reports only files inside the
    /// identity's `scope.read_paths` and the directories that lead to them.
    pub fn scoped(workspace_root: PathBuf, scope: Option<PathScope>) -> Self {
        Self {
            workspace_root,
            scope,
        }
    }

    fn readable(&self, path: &Path, root: &Path) -> bool {
        match &self.scope {
            Some(scope) => scope.permits(PathAccess::Read, relative_to(path, root)),
            None => true,
        }
    }

    fn leads_to_readable(&self, dir: &Path, root: &Path) -> bool {
        match &self.scope {
            Some(scope) => scope.contains_readable(dir, root),
            None => true,
        }
    }

    fn list_recursive(
        &self,
        dir: &Path,
        root: &Path,
        pattern: Option<&str>,
        entries: &mut Vec<Value>,
    ) -> ToolResult<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;
            let name = entry.file_name().to_string_lossy().to_string();
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();

            if file_type.is_dir() {
                self.list_recursive(&path, root, pattern, entries)?;
            } else {
                if !self.readable(&path, root) {
                    continue;
                }
                if let Some(pat) = pattern {
                    if !glob::Pattern::new(pat)
                        .map_err(|e| ToolError::InvalidArguments {
                            message: format!("Invalid glob pattern: {}", e),
                        })?
                        .matches(&name)
                    {
                        continue;
                    }
                }

                entries.push(json!({
                    "name": name,
                    "path": relative,
                    "is_dir": false,
                    "is_file": true,
                }));
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for ListDirTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "list_directory".to_string(),
                description: "List files and directories in a path.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "path".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Path to list (relative to workspace root, defaults to '.')"
                                        .to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "recursive".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Boolean,
                                description: Some(
                                    "Whether to list recursively (default: false)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "pattern".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Glob pattern to filter files (e.g., '*.rs')".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec![]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

        let path = Path::new(path_str);
        let root = canonical_root(&self.workspace_root)?;
        let safe_path = validate_path_within_workspace(path, &self.workspace_root)?;

        let recursive = args
            .get("recursive")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let pattern = args.get("pattern").and_then(|v| v.as_str());

        let mut entries = Vec::new();

        if recursive {
            self.list_recursive(&safe_path, &root, pattern, &mut entries)?;
        } else {
            for entry in std::fs::read_dir(&safe_path)? {
                let entry = entry?;
                let file_type = entry.file_type()?;
                let name = entry.file_name().to_string_lossy().to_string();
                let visible = if file_type.is_dir() {
                    self.leads_to_readable(&entry.path(), &root)
                } else {
                    self.readable(&entry.path(), &root)
                };
                if !visible {
                    continue;
                }

                if let Some(pat) = pattern {
                    if !glob::Pattern::new(pat)
                        .map_err(|e| ToolError::InvalidArguments {
                            message: format!("Invalid glob pattern: {}", e),
                        })?
                        .matches(&name)
                    {
                        continue;
                    }
                }

                entries.push(json!({
                    "name": name,
                    "is_dir": file_type.is_dir(),
                    "is_file": file_type.is_file(),
                }));
            }
        }

        Ok(json!({
            "path": path_str,
            "entries": entries,
            "count": entries.len()
        }))
    }

    fn name(&self) -> &str {
        "list_directory"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::None
    }
}

pub struct SearchTool {
    workspace_root: PathBuf,
    scope: Option<PathScope>,
}

impl SearchTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self::scoped(workspace_root, None)
    }

    /// A searcher that, when `scope` is present, reads only files inside the
    /// identity's `scope.read_paths`.
    pub fn scoped(workspace_root: PathBuf, scope: Option<PathScope>) -> Self {
        Self {
            workspace_root,
            scope,
        }
    }

    fn readable(&self, path: &Path, root: &Path) -> bool {
        match &self.scope {
            Some(scope) => scope.permits(PathAccess::Read, relative_to(path, root)),
            None => true,
        }
    }

    fn search_recursive(
        &self,
        dir: &Path,
        root: &Path,
        regex: &regex::Regex,
        file_pattern: Option<&str>,
        max_results: usize,
        results: &mut Vec<Value>,
    ) -> ToolResult<()> {
        if results.len() >= max_results {
            return Ok(());
        }

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let file_type = entry.file_type()?;

            if file_type.is_dir() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || name == "target" || name == "node_modules" {
                    continue;
                }
                self.search_recursive(&path, root, regex, file_pattern, max_results, results)?;
            } else if file_type.is_file() {
                if !self.readable(&path, root) {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();

                if let Some(pat) = file_pattern {
                    if !glob::Pattern::new(pat)
                        .map_err(|e| ToolError::InvalidArguments {
                            message: format!("Invalid glob pattern: {}", e),
                        })?
                        .matches(&name)
                    {
                        continue;
                    }
                }

                if let Ok(content) = std::fs::read_to_string(&path) {
                    let relative = relative_to(&path, root).to_string_lossy().to_string();

                    for (line_num, line) in content.lines().enumerate() {
                        if results.len() >= max_results {
                            return Ok(());
                        }

                        if regex.is_match(line) {
                            results.push(json!({
                                "file": relative,
                                "line": line_num + 1,
                                "content": line,
                            }));
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Tool for SearchTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "search".to_string(),
                description: "Search for a pattern in files. Returns matching lines with context."
                    .to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "pattern".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some("Regex pattern to search for".to_string()),
                                items: None,
                            },
                        );
                        props.insert(
                            "path".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Path to search in (defaults to workspace root)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "file_pattern".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Glob pattern to filter files (e.g., '*.rs')".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "max_results".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Integer,
                                description: Some(
                                    "Maximum number of results to return (default: 50)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["pattern".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let pattern_str = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "Missing or invalid 'pattern' parameter".to_string(),
            })?;

        let regex = regex::Regex::new(pattern_str).map_err(|e| ToolError::InvalidArguments {
            message: format!("Invalid regex pattern: {}", e),
        })?;

        let path_str = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let path = Path::new(path_str);
        let root = canonical_root(&self.workspace_root)?;
        let dir = validate_path_within_workspace(path, &self.workspace_root)?;

        let file_pattern = args.get("file_pattern").and_then(|v| v.as_str());
        let max_results = args
            .get("max_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(50) as usize;

        let mut results = Vec::new();
        self.search_recursive(&dir, &root, &regex, file_pattern, max_results, &mut results)?;

        Ok(json!({
            "pattern": pattern_str,
            "results": results,
            "count": results.len()
        }))
    }

    fn name(&self) -> &str {
        "search"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::None
    }
}

pub struct GitStatusTool {
    workspace_root: PathBuf,
}

impl GitStatusTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for GitStatusTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "git_status".to_string(),
                description: "Get the current git repository status including branch, staged files, and modified files.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some(HashMap::new()),
                    required: Some(vec![]),
                },
            },
        }
    }

    async fn execute(&self, _args: Value) -> ToolResult<Value> {
        use crate::entities::git::GitRepository;

        let repo = GitRepository::detect(&self.workspace_root).ok_or_else(|| {
            ToolError::ExecutionFailed {
                message: "Not a git repository".to_string(),
            }
        })?;

        Ok(json!({
            "branch": repo.current_branch,
            "commit": repo.head_commit,
            "is_dirty": repo.is_dirty,
            "staged_files": repo.staged_files,
            "modified_files": repo.modified_files,
            "untracked_files": repo.untracked_files,
            "summary": repo.summary()
        }))
    }

    fn name(&self) -> &str {
        "git_status"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::None
    }
}

pub struct GitDiffTool {
    workspace_root: PathBuf,
}

impl GitDiffTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for GitDiffTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "git_diff".to_string(),
                description: "Show git diff for files. Can show staged or unstaged changes."
                    .to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "path".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Path to diff (optional, defaults to all files)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "staged".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::Boolean,
                                description: Some(
                                    "Show staged changes instead of unstaged (default: false)"
                                        .to_string(),
                                ),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec![]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let path = args.get("path").and_then(|v| v.as_str());
        let staged = args
            .get("staged")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut cmd = std::process::Command::new("git");
        cmd.current_dir(&self.workspace_root);

        if staged {
            cmd.args(["diff", "--cached"]);
        } else {
            cmd.arg("diff");
        }

        if let Some(p) = path {
            cmd.arg("--").arg(p);
        }

        let output = cmd.output().map_err(|e| ToolError::ExecutionFailed {
            message: format!("Failed to run git diff: {}", e),
        })?;

        if !output.status.success() {
            return Err(ToolError::ExecutionFailed {
                message: String::from_utf8_lossy(&output.stderr).to_string(),
            });
        }

        let diff = String::from_utf8_lossy(&output.stdout).to_string();

        Ok(json!({
            "diff": diff,
            "staged": staged,
            "path": path.unwrap_or("(all files)"),
            "has_changes": !diff.is_empty()
        }))
    }

    fn name(&self) -> &str {
        "git_diff"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::None
    }
}

pub struct RunCommandTool {
    container_handle: std::sync::Arc<crate::container::ContainerHandle>,
    working_dir: Option<String>,
}

impl RunCommandTool {
    pub fn new(
        container_handle: std::sync::Arc<crate::container::ContainerHandle>,
        working_dir: Option<String>,
    ) -> Self {
        Self {
            container_handle,
            working_dir,
        }
    }
}

#[async_trait]
impl Tool for RunCommandTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "run_command".to_string(),
                description: "Run a shell command in the dev container workspace.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "command".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "The shell command to run (passed to sh -c)".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec!["command".to_string()]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "Missing or invalid 'command' parameter".to_string(),
            })?;

        let result = crate::container::exec_in_container(
            &self.container_handle,
            &["sh", "-c", command],
            self.working_dir.as_deref(),
        )
        .map_err(|e| ToolError::ExecutionFailed {
            message: e.to_string(),
        })?;

        Ok(json!({
            "stdout": result.stdout,
            "stderr": result.stderr,
            "success": result.success,
        }))
    }

    fn name(&self) -> &str {
        "run_command"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Workspace
    }
}

/// GitHub API connection status for transparent degradation.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum GitHubStatus {
    /// Successfully connected to GitHub API.
    Connected,
    /// No GITHUB_TOKEN environment variable configured.
    #[default]
    NoToken,
    /// API call failed with an error message.
    ApiError(String),
}

/// GitHub PR status data collected from git and GitHub REST API.
#[derive(Debug, Clone, Default)]
pub struct PrStatusData {
    /// PR number (e.g., "#42")
    pub pr_number: Option<u64>,
    /// Linked issue number (e.g., "#17")
    pub issue_number: Option<u64>,
    /// PR status: "draft", "ready", "merged", "closed"
    pub pr_status: Option<String>,
    /// Review state: "approved", "changes-requested", "review-required"
    pub review_state: Option<String>,
    /// Number of files with merge conflicts
    pub conflict_count: Option<usize>,
    /// List of conflicting file paths
    pub conflict_files: Vec<String>,
    /// Commits ahead of upstream
    pub ahead: Option<usize>,
    /// Commits behind upstream
    pub behind: Option<usize>,
    /// Lines added
    pub additions: Option<usize>,
    /// Lines deleted
    pub deletions: Option<usize>,
    /// CI status: "pass", "fail", "pending"
    pub ci_status: Option<String>,
    /// Failing CI check names
    pub ci_failing_checks: Vec<String>,
    /// Whether automerge is enabled
    pub automerge: bool,
    /// Days since last update (staleness)
    pub staleness_days: Option<u64>,
    /// Current branch name
    pub branch: Option<String>,
    /// Short HEAD commit SHA (fallback when no PR)
    pub head_sha: Option<String>,
    /// Whether this branch has an upstream
    pub has_upstream: bool,
    /// Changed file paths (for diff detail)
    pub changed_files: Vec<String>,
    /// GitHub API connection status
    pub github_status: GitHubStatus,
}

impl PrStatusData {
    /// Format as L0 compact single-line status.
    ///
    /// Only includes salient (non-default) fields. Omitted fields represent
    /// non-salient states (e.g., no conflicts, automerge disabled).
    pub fn to_l0(&self) -> String {
        let mut parts = Vec::new();

        // PR number or commit SHA as context anchor
        if let Some(pr) = self.pr_number {
            parts.push(format!("#{}", pr));
        } else if let Some(ref sha) = self.head_sha {
            parts.push(sha.clone());
        }

        // Linked issue
        if let Some(issue) = self.issue_number {
            parts.push(format!("#{}", issue));
        }

        // PR status (only show draft, since "ready" is implied if reviews exist)
        if let Some(ref status) = self.pr_status {
            match status.as_str() {
                "draft" => parts.push("draft".to_string()),
                "merged" => parts.push("merged".to_string()),
                "closed" => parts.push("closed".to_string()),
                "ready" => parts.push("ready".to_string()),
                _ => {}
            }
        }

        // Review state (only show when salient)
        if let Some(ref review) = self.review_state {
            match review.as_str() {
                "approved" => parts.push("approved".to_string()),
                "changes-requested" => parts.push("changes-requested".to_string()),
                _ => {}
            }
        }

        // Merge conflicts
        if let Some(count) = self.conflict_count {
            if count > 0 {
                parts.push(format!("conflicts:{}", count));
            }
        }

        // Sync state (ahead/behind)
        if !self.has_upstream {
            parts.push("no-upstream".to_string());
        } else {
            if let Some(behind) = self.behind {
                if behind > 0 {
                    parts.push(format!("behind:{}", behind));
                }
            }
            if let Some(ahead) = self.ahead {
                if ahead > 0 {
                    parts.push(format!("ahead:{}", ahead));
                }
            }
        }

        // Diff stats
        if let (Some(a), Some(d)) = (self.additions, self.deletions) {
            parts.push(format!("+{}/-{}", a, d));
        }

        // CI status
        if let Some(ref ci) = self.ci_status {
            match ci.as_str() {
                "pass" => parts.push("ci:pass".to_string()),
                "fail" => parts.push("ci:fail".to_string()),
                "pending" => parts.push("ci:pending".to_string()),
                _ => {}
            }
        }

        // Automerge (only show when enabled, since disabled is default)
        if self.automerge {
            parts.push("automerge".to_string());
        }

        // Staleness
        if let Some(days) = self.staleness_days {
            if days > 0 {
                parts.push(format!("{}d", days));
            }
        }

        // GitHub connection status (visible degradation)
        match &self.github_status {
            GitHubStatus::Connected => {}
            GitHubStatus::NoToken => parts.push("[github:unconfigured]".to_string()),
            GitHubStatus::ApiError(_) => parts.push("[github:error]".to_string()),
        }

        parts.join(" ")
    }

    /// Format L1 detail for a specific field.
    pub fn to_l1(&self, field: &str) -> Result<String, String> {
        match field {
            "conflicts" => {
                if self.conflict_files.is_empty() {
                    Ok("No merge conflicts.".to_string())
                } else {
                    Ok(format!(
                        "Conflicting files ({}):\n{}",
                        self.conflict_files.len(),
                        self.conflict_files
                            .iter()
                            .map(|f| format!("  - {}", f))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ))
                }
            }
            "ci" => {
                let status = self.ci_status.as_deref().unwrap_or("unknown");
                if self.ci_failing_checks.is_empty() {
                    Ok(format!("CI status: {}", status))
                } else {
                    Ok(format!(
                        "CI status: {}\nFailing checks:\n{}",
                        status,
                        self.ci_failing_checks
                            .iter()
                            .map(|c| format!("  - {}", c))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ))
                }
            }
            "diff" => {
                let stats = match (self.additions, self.deletions) {
                    (Some(a), Some(d)) => format!("+{}/-{}", a, d),
                    _ => "no diff data".to_string(),
                };
                if self.changed_files.is_empty() {
                    Ok(format!("Diff: {}", stats))
                } else {
                    Ok(format!(
                        "Diff: {}\nChanged files ({}):\n{}",
                        stats,
                        self.changed_files.len(),
                        self.changed_files
                            .iter()
                            .map(|f| format!("  - {}", f))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ))
                }
            }
            "sync" => {
                if !self.has_upstream {
                    Ok("No upstream tracking branch configured.".to_string())
                } else {
                    let ahead = self.ahead.unwrap_or(0);
                    let behind = self.behind.unwrap_or(0);
                    Ok(format!(
                        "Sync: {} ahead, {} behind upstream",
                        ahead, behind
                    ))
                }
            }
            "review" => {
                let state = self.review_state.as_deref().unwrap_or("none");
                Ok(format!("Review state: {}", state))
            }
            "automerge" => Ok(format!(
                "Automerge: {}",
                if self.automerge {
                    "enabled"
                } else {
                    "disabled"
                }
            )),
            "staleness" => match self.staleness_days {
                Some(days) => Ok(format!("Last updated {} days ago", days)),
                None => Ok("Staleness data not available.".to_string()),
            },
            "github" => match &self.github_status {
                GitHubStatus::Connected => {
                    Ok("GitHub API: connected (token configured)".to_string())
                }
                GitHubStatus::NoToken => Ok(
                    "GitHub API: not configured. Set GITHUB_TOKEN env var with repo:status and read:org scopes to enable PR data, CI status, and review information.".to_string(),
                ),
                GitHubStatus::ApiError(msg) => {
                    Ok(format!("GitHub API: error — {}", msg))
                }
            },
            _ => Err(format!(
                "Unknown field '{}'. Valid fields: conflicts, ci, diff, sync, review, automerge, staleness, github",
                field
            )),
        }
    }
}

/// Collect PR status data from git and (optionally) the GitHub REST API.
fn collect_pr_status(workspace_root: &Path) -> ToolResult<PrStatusData> {
    let mut data = PrStatusData::default();

    // -- Git-based data --

    // Branch and HEAD SHA
    let branch_output = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(workspace_root)
        .output()
        .map_err(|e| ToolError::ExecutionFailed {
            message: format!("Failed to get branch: {}", e),
        })?;
    if branch_output.status.success() {
        let branch = String::from_utf8_lossy(&branch_output.stdout)
            .trim()
            .to_string();
        if !branch.is_empty() {
            data.branch = Some(branch);
        }
    }

    let sha_output = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(workspace_root)
        .output()
        .map_err(|e| ToolError::ExecutionFailed {
            message: format!("Failed to get HEAD SHA: {}", e),
        })?;
    if sha_output.status.success() {
        data.head_sha = Some(
            String::from_utf8_lossy(&sha_output.stdout)
                .trim()
                .to_string(),
        );
    }

    // Ahead/behind upstream
    let tracking_output = std::process::Command::new("git")
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .current_dir(workspace_root)
        .output();

    match tracking_output {
        Ok(ref output) if output.status.success() => {
            data.has_upstream = true;
            let counts = String::from_utf8_lossy(&output.stdout);
            let parts: Vec<&str> = counts.trim().split('\t').collect();
            if parts.len() == 2 {
                data.ahead = parts[0].parse().ok();
                data.behind = parts[1].parse().ok();
            }
        }
        _ => {
            data.has_upstream = false;
        }
    }

    // Diff stats against upstream (or default branch)
    let diff_base = if data.has_upstream {
        "@{upstream}".to_string()
    } else {
        // Try origin/main, then origin/master
        let main_check = std::process::Command::new("git")
            .args(["rev-parse", "--verify", "origin/main"])
            .current_dir(workspace_root)
            .output();
        if main_check.map(|o| o.status.success()).unwrap_or(false) {
            "origin/main".to_string()
        } else {
            "origin/master".to_string()
        }
    };

    let diff_stat_output = std::process::Command::new("git")
        .args(["diff", "--stat", &diff_base])
        .current_dir(workspace_root)
        .output();

    if let Ok(ref output) = diff_stat_output {
        if output.status.success() {
            let stat_text = String::from_utf8_lossy(&output.stdout);
            // Parse "X files changed, Y insertions(+), Z deletions(-)" from last line
            if let Some(last_line) = stat_text.lines().last() {
                let mut additions = 0usize;
                let mut deletions = 0usize;
                for part in last_line.split(',') {
                    let part = part.trim();
                    if part.contains("insertion") {
                        if let Some(n) = part.split_whitespace().next() {
                            additions = n.parse().unwrap_or(0);
                        }
                    } else if part.contains("deletion") {
                        if let Some(n) = part.split_whitespace().next() {
                            deletions = n.parse().unwrap_or(0);
                        }
                    }
                }
                data.additions = Some(additions);
                data.deletions = Some(deletions);
            }
        }
    }

    // Changed files list (for L1 diff detail)
    let diff_files_output = std::process::Command::new("git")
        .args(["diff", "--name-only", &diff_base])
        .current_dir(workspace_root)
        .output();

    if let Ok(ref output) = diff_files_output {
        if output.status.success() {
            data.changed_files = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();
        }
    }

    // Merge conflicts (check for unmerged paths)
    let conflict_output = std::process::Command::new("git")
        .args(["diff", "--name-only", "--diff-filter=U"])
        .current_dir(workspace_root)
        .output();

    if let Ok(ref output) = conflict_output {
        if output.status.success() {
            let files: Vec<String> = String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();
            if !files.is_empty() {
                data.conflict_count = Some(files.len());
                data.conflict_files = files;
            }
        }
    }

    // -- GitHub REST API data (explicit degradation) --
    let token = std::env::var("GITHUB_TOKEN").ok();

    if let Some(ref token) = token {
        match fetch_github_pr_data(workspace_root, &data, token) {
            Ok(gh_data) => {
                data.pr_number = gh_data.pr_number.or(data.pr_number);
                data.issue_number = gh_data.issue_number.or(data.issue_number);
                data.pr_status = gh_data.pr_status.or(data.pr_status);
                data.review_state = gh_data.review_state.or(data.review_state);
                data.ci_status = gh_data.ci_status.or(data.ci_status);
                data.ci_failing_checks = if gh_data.ci_failing_checks.is_empty() {
                    data.ci_failing_checks
                } else {
                    gh_data.ci_failing_checks
                };
                data.automerge = gh_data.automerge;
                data.staleness_days = gh_data.staleness_days.or(data.staleness_days);
                if let Some(count) = gh_data.conflict_count {
                    if data.conflict_count.is_none() {
                        data.conflict_count = Some(count);
                    }
                }
                data.github_status = GitHubStatus::Connected;
            }
            Err(e) => {
                data.github_status = GitHubStatus::ApiError(e);
            }
        }
    }
    // else: data.github_status remains NoToken (the default)

    Ok(data)
}

/// Parse a GitHub remote URL into (owner, repo).
fn parse_github_remote(url: &str) -> Option<(String, String)> {
    // Handle SSH: git@github.com:owner/repo.git
    if let Some(rest) = url.strip_prefix("git@github.com:") {
        let path = rest.trim_end_matches(".git");
        let (owner, repo) = path.split_once('/')?;
        if !owner.is_empty() && !repo.is_empty() {
            return Some((owner.to_string(), repo.to_string()));
        }
    }
    // Handle HTTPS: https://github.com/owner/repo.git
    if url.contains("github.com") {
        let path = url
            .split("github.com")
            .nth(1)?
            .trim_start_matches('/')
            .trim_start_matches(':')
            .trim_end_matches(".git");
        let (owner, repo) = path.split_once('/')?;
        if !owner.is_empty() && !repo.is_empty() {
            return Some((owner.to_string(), repo.to_string()));
        }
    }
    None
}

/// Fetch PR data from the GitHub REST API. Returns partial data on success,
/// or an error message string on failure.
fn fetch_github_pr_data(
    workspace_root: &Path,
    local_data: &PrStatusData,
    token: &str,
) -> Result<PrStatusData, String> {
    let mut gh_data = PrStatusData::default();

    // Get remote URL
    let remote_output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(workspace_root)
        .output()
        .map_err(|e| format!("failed to get git remote: {}", e))?;

    if !remote_output.status.success() {
        return Err("no 'origin' remote configured".to_string());
    }

    let remote_url = String::from_utf8_lossy(&remote_output.stdout)
        .trim()
        .to_string();
    let (owner, repo) =
        parse_github_remote(&remote_url).ok_or_else(|| "not a GitHub remote".to_string())?;

    let branch = local_data
        .branch
        .as_deref()
        .ok_or_else(|| "no branch detected".to_string())?;

    let client = reqwest::blocking::Client::new();
    let api_base = "https://api.github.com";

    // Find PR for current branch
    let pr_url = format!(
        "{}/repos/{}/{}/pulls?head={}:{}&state=open",
        api_base, owner, repo, owner, branch
    );
    let pr_resp = client
        .get(&pr_url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "nanna-coder-harness")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .map_err(|e| format!("GitHub API request failed: {}", e))?;

    if !pr_resp.status().is_success() {
        let status = pr_resp.status();
        return Err(format!("GitHub API returned {}", status));
    }

    let prs: Vec<Value> = pr_resp
        .json()
        .map_err(|e| format!("failed to parse PR response: {}", e))?;

    let pr_json = match prs.first() {
        Some(pr) => pr,
        None => return Ok(gh_data), // No open PR for this branch — not an error
    };

    // PR number
    gh_data.pr_number = pr_json.get("number").and_then(|v| v.as_u64());

    // PR status (draft/ready/merged/closed)
    let is_draft = pr_json
        .get("draft")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let state = pr_json.get("state").and_then(|v| v.as_str()).unwrap_or("");
    gh_data.pr_status = Some(match (state, is_draft) {
        (_, true) => "draft".to_string(),
        ("closed", _) => "closed".to_string(),
        _ => "ready".to_string(),
    });

    // Merge conflicts from mergeable_state
    if let Some(mergeable_state) = pr_json.get("mergeable_state").and_then(|v| v.as_str()) {
        if mergeable_state == "dirty" {
            gh_data.conflict_count = Some(1);
        }
    }

    // Automerge
    gh_data.automerge = pr_json
        .get("auto_merge")
        .map(|v| !v.is_null())
        .unwrap_or(false);

    // Staleness
    if let Some(updated_at) = pr_json.get("updated_at").and_then(|v| v.as_str()) {
        if let Ok(updated) = chrono::DateTime::parse_from_rfc3339(updated_at) {
            let now = chrono::Utc::now();
            let duration = now.signed_duration_since(updated);
            gh_data.staleness_days = Some(duration.num_days() as u64);
        }
    }

    // Linked issues from body (look for "Closes #N" / "Fixes #N" patterns)
    if let Some(body) = pr_json.get("body").and_then(|v| v.as_str()) {
        let issue_re =
            regex::Regex::new(r"(?i)(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s+#(\d+)").ok();
        if let Some(re) = issue_re {
            if let Some(caps) = re.captures(body) {
                if let Some(num) = caps.get(1).and_then(|m| m.as_str().parse::<u64>().ok()) {
                    gh_data.issue_number = Some(num);
                }
            }
        }
    }

    let pr_number = match gh_data.pr_number {
        Some(n) => n,
        None => return Ok(gh_data),
    };

    // Fetch reviews for review decision
    let reviews_url = format!(
        "{}/repos/{}/{}/pulls/{}/reviews",
        api_base, owner, repo, pr_number
    );
    if let Ok(reviews_resp) = client
        .get(&reviews_url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", "nanna-coder-harness")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
    {
        if reviews_resp.status().is_success() {
            if let Ok(reviews) = reviews_resp.json::<Vec<Value>>() {
                // Use the last substantive review state per reviewer
                let mut latest_states: HashMap<String, String> = HashMap::new();
                for review in &reviews {
                    let user = review
                        .get("user")
                        .and_then(|u| u.get("login"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let state = review.get("state").and_then(|v| v.as_str()).unwrap_or("");
                    if state == "APPROVED" || state == "CHANGES_REQUESTED" || state == "DISMISSED" {
                        latest_states.insert(user.to_string(), state.to_string());
                    }
                }
                if latest_states.values().any(|s| s == "CHANGES_REQUESTED") {
                    gh_data.review_state = Some("changes-requested".to_string());
                } else if latest_states.values().any(|s| s == "APPROVED") {
                    gh_data.review_state = Some("approved".to_string());
                } else if !latest_states.is_empty() {
                    gh_data.review_state = Some("review-required".to_string());
                }
            }
        }
    }

    // Fetch CI status via check-runs
    let head_sha = pr_json
        .get("head")
        .and_then(|h| h.get("sha"))
        .and_then(|v| v.as_str());
    if let Some(sha) = head_sha {
        let checks_url = format!(
            "{}/repos/{}/{}/commits/{}/check-runs",
            api_base, owner, repo, sha
        );
        if let Ok(checks_resp) = client
            .get(&checks_url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept", "application/vnd.github+json")
            .header("User-Agent", "nanna-coder-harness")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
        {
            if checks_resp.status().is_success() {
                if let Ok(checks_json) = checks_resp.json::<Value>() {
                    if let Some(check_runs) =
                        checks_json.get("check_runs").and_then(|v| v.as_array())
                    {
                        let mut has_fail = false;
                        let mut has_pending = false;
                        let mut failing_names = Vec::new();

                        for check in check_runs {
                            let conclusion = check
                                .get("conclusion")
                                .and_then(|v| v.as_str())
                                .unwrap_or("");
                            let status = check.get("status").and_then(|v| v.as_str()).unwrap_or("");
                            let name = check
                                .get("name")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");

                            if conclusion == "failure" || conclusion == "timed_out" {
                                has_fail = true;
                                failing_names.push(name.to_string());
                            } else if status == "queued"
                                || status == "in_progress"
                                || status == "waiting"
                            {
                                has_pending = true;
                            }
                        }

                        gh_data.ci_status = Some(if has_fail {
                            "fail".to_string()
                        } else if has_pending {
                            "pending".to_string()
                        } else {
                            "pass".to_string()
                        });
                        gh_data.ci_failing_checks = failing_names;
                    }
                }
            }
        }
    }

    Ok(gh_data)
}

pub struct GitHubPrStatusTool {
    workspace_root: PathBuf,
}

impl GitHubPrStatusTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for GitHubPrStatusTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            function: FunctionDefinition {
                name: "github_pr_status".to_string(),
                description: "Get GitHub PR status. L0: compact single-line status. L1: detailed expansion of a specific field.".to_string(),
                parameters: JsonSchema {
                    schema_type: SchemaType::Object,
                    properties: Some({
                        let mut props = HashMap::new();
                        props.insert(
                            "level".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Detail level: 'l0' for compact status line (default), 'l1' for expanded field detail".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props.insert(
                            "field".to_string(),
                            PropertySchema {
                                schema_type: SchemaType::String,
                                description: Some(
                                    "Field to expand (required for l1). Options: conflicts, ci, diff, sync, review, automerge, staleness, github".to_string(),
                                ),
                                items: None,
                            },
                        );
                        props
                    }),
                    required: Some(vec![]),
                },
            },
        }
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let level = args.get("level").and_then(|v| v.as_str()).unwrap_or("l0");

        let data = collect_pr_status(&self.workspace_root)?;
        let github_connected = data.github_status == GitHubStatus::Connected;

        match level {
            "l0" => {
                let status_line = data.to_l0();
                Ok(json!({
                    "level": "l0",
                    "status": status_line,
                    "github_connected": github_connected
                }))
            }
            "l1" => {
                let field = args.get("field").and_then(|v| v.as_str()).ok_or_else(|| {
                    ToolError::InvalidArguments {
                        message: "Missing 'field' parameter for l1 query".to_string(),
                    }
                })?;

                let detail = data
                    .to_l1(field)
                    .map_err(|e| ToolError::InvalidArguments { message: e })?;

                Ok(json!({
                    "level": "l1",
                    "field": field,
                    "detail": detail,
                    "github_connected": github_connected
                }))
            }
            _ => Err(ToolError::InvalidArguments {
                message: format!("Invalid level '{}'. Use 'l0' or 'l1'.", level),
            }),
        }
    }

    fn name(&self) -> &str {
        "github_pr_status"
    }

    fn effect_class(&self) -> EffectClass {
        EffectClass::Repository
    }
}

pub fn create_tool_registry(workspace_root: &std::path::Path) -> ToolRegistry {
    create_tool_registry_with_scope(workspace_root, None, UNSCOPED_IDENTITY)
}

/// The default tools restricted to `identity`: file tools carry the
/// identity's [`PathScope`] and the registry is [`ToolRegistry::scoped_for`]
/// the identity.
pub fn create_tool_registry_for(
    workspace_root: &std::path::Path,
    identity: &AgentIdentity,
) -> Result<ToolRegistry, ScopeError> {
    let scope = PathScope::from_identity(identity)?;
    Ok(
        create_tool_registry_with_scope(workspace_root, Some(scope), identity.name())
            .scoped_for(identity),
    )
}

fn create_tool_registry_with_scope(
    workspace_root: &std::path::Path,
    scope: Option<PathScope>,
    identity_name: &str,
) -> ToolRegistry {
    let root = workspace_root.to_path_buf();
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(EchoTool::new()));
    registry.register(Box::new(CalculatorTool::new()));
    registry.register(Box::new(ReadFileTool::scoped(root.clone(), scope.clone())));
    registry.register(Box::new(WriteFileTool::scoped(root.clone(), scope.clone())));
    registry.register(Box::new(ListDirTool::scoped(root.clone(), scope.clone())));
    registry.register(Box::new(SearchTool::scoped(root, scope)));
    registry.register(Box::new(GitStatusTool::new(workspace_root.to_path_buf())));
    registry.register(Box::new(GitDiffTool::new(workspace_root.to_path_buf())));
    registry.register(Box::new(GitHubPrStatusTool::new(
        workspace_root.to_path_buf(),
    )));
    crate::pr_tools::register(&mut registry, workspace_root, identity_name);
    registry
}

/// The working directory inside the dev container where the worktree is mounted.
pub const CONTAINER_WORKSPACE_DIR: &str = "/workspace";

pub fn create_container_tool_registry(
    workspace_root: &std::path::Path,
    container_handle: std::sync::Arc<crate::container::ContainerHandle>,
    container_working_dir: &str,
) -> ToolRegistry {
    let mut registry = create_tool_registry(workspace_root);
    // Deliberately overrides any `run_command` entry from `create_tool_registry`
    // with a container-bound version; if `create_tool_registry` ever adds a
    // `run_command` tool, this override is intentional and expected.
    registry.register(Box::new(RunCommandTool::new(
        container_handle,
        Some(container_working_dir.to_string()),
    )));
    registry
}

/// The container-bound tools restricted to `identity`. `run_command`
/// survives the scoping only when `scope.tools` names it and the ceiling is
/// at least [`EffectClass::Workspace`], the class it declares.
pub fn create_container_tool_registry_for(
    workspace_root: &std::path::Path,
    container_handle: std::sync::Arc<crate::container::ContainerHandle>,
    container_working_dir: &str,
    identity: &AgentIdentity,
) -> Result<ToolRegistry, ScopeError> {
    let scope = PathScope::from_identity(identity)?;
    let mut registry =
        create_tool_registry_with_scope(workspace_root, Some(scope), identity.name());
    let working_dir = Some(container_working_dir.to_string());
    registry.register(Box::new(RunCommandTool::new(container_handle, working_dir)));
    Ok(registry.scoped_for(identity))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::DenialReason;
    use proptest::prelude::*;

    struct StubTool {
        name: String,
        class: EffectClass,
    }

    impl StubTool {
        fn boxed(name: &str, class: EffectClass) -> Box<dyn Tool> {
            Box::new(Self {
                name: name.to_string(),
                class,
            })
        }
    }

    #[async_trait]
    impl Tool for StubTool {
        fn definition(&self) -> ToolDefinition {
            ToolDefinition {
                function: FunctionDefinition {
                    name: self.name.clone(),
                    description: String::new(),
                    parameters: JsonSchema {
                        schema_type: SchemaType::Object,
                        properties: None,
                        required: None,
                    },
                },
            }
        }

        async fn execute(&self, _args: Value) -> ToolResult<Value> {
            Ok(json!({ "class": self.class.as_str() }))
        }

        fn name(&self) -> &str {
            &self.name
        }

        fn effect_class(&self) -> EffectClass {
            self.class
        }
    }

    fn names(tools: &[&dyn Tool]) -> Vec<String> {
        tools.iter().map(|tool| tool.name().to_string()).collect()
    }

    const EXPECTED_CLASSES: [(&str, EffectClass); 10] = [
        ("calculate", EffectClass::None),
        ("echo", EffectClass::None),
        ("git_diff", EffectClass::None),
        ("git_push_branch", EffectClass::Repository),
        ("git_status", EffectClass::None),
        ("github_pr_status", EffectClass::Repository),
        ("list_directory", EffectClass::None),
        ("read_file", EffectClass::None),
        ("search", EffectClass::None),
        ("write_file", EffectClass::Workspace),
    ];

    #[test]
    fn every_default_tool_declares_the_expected_effect_class() {
        let registry = create_tool_registry(Path::new("."));
        let mut registered = registry.list_tools();
        registered.sort_unstable();
        let expected: Vec<&str> = EXPECTED_CLASSES.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            registered, expected,
            "every registered tool must be classified"
        );
        for (name, class) in EXPECTED_CLASSES {
            assert_eq!(registry.effect_class_of(name), Some(class), "{name}");
        }
    }

    #[test]
    fn run_command_declares_the_maximum_class_it_can_reach() {
        let handle = std::sync::Arc::new(crate::container::ContainerHandle {
            name: "effects-test-container".to_string(),
            runtime: crate::container::ContainerRuntime::None,
            port: None,
            needs_cleanup: false,
        });
        let registry =
            create_container_tool_registry(Path::new("."), handle, CONTAINER_WORKSPACE_DIR);
        assert_eq!(
            registry.effect_class_of("run_command"),
            Some(EffectClass::Workspace)
        );
        let workspace_names = names(&registry.at_most(EffectClass::Workspace));
        assert!(workspace_names.contains(&"run_command".to_string()));
        assert!(!workspace_names.contains(&"github_pr_status".to_string()));
    }

    #[test]
    fn at_most_workspace_excludes_repository_tools() {
        let registry = create_tool_registry(Path::new("."));
        let allowed = names(&registry.at_most(EffectClass::Workspace));
        assert_eq!(
            allowed,
            vec![
                "calculate",
                "echo",
                "git_diff",
                "git_status",
                "list_directory",
                "read_file",
                "search",
                "write_file",
            ]
        );
        let everything = names(&registry.at_most(EffectClass::Production));
        assert_eq!(everything.len(), EXPECTED_CLASSES.len());
    }

    #[test]
    fn with_class_and_by_class_partition_the_registry() {
        let mut registry = ToolRegistry::new();
        registry.register(StubTool::boxed("deploy", EffectClass::Production));
        registry.register(StubTool::boxed("ls", EffectClass::None));
        registry.register(StubTool::boxed("cat", EffectClass::None));
        registry.register(StubTool::boxed("push", EffectClass::Repository));

        assert_eq!(
            names(&registry.with_class(EffectClass::None)),
            vec!["cat", "ls"]
        );
        assert_eq!(
            names(&registry.with_class(EffectClass::Ci)),
            Vec::<String>::new()
        );

        let grouped = registry.by_class();
        assert_eq!(grouped.len(), EffectClass::ALL.len());
        assert_eq!(grouped[&EffectClass::None], vec!["cat", "ls"]);
        assert_eq!(grouped[&EffectClass::Workspace], Vec::<&str>::new());
        assert_eq!(grouped[&EffectClass::Repository], vec!["push"]);
        assert_eq!(grouped[&EffectClass::Production], vec!["deploy"]);
    }

    #[tokio::test]
    async fn retain_at_most_drops_tools_above_the_ceiling() {
        let mut registry = ToolRegistry::new();
        registry.register(StubTool::boxed("deploy", EffectClass::Production));
        registry.register(StubTool::boxed("edit", EffectClass::Workspace));
        registry.register(StubTool::boxed("ls", EffectClass::None));

        registry.retain_at_most(EffectClass::Workspace);

        let mut remaining = registry.list_tools();
        remaining.sort_unstable();
        assert_eq!(remaining, vec!["edit", "ls"]);
        assert!(matches!(
            registry.execute("deploy", json!({})).await,
            Err(ToolError::NotFound { .. })
        ));
        assert_eq!(
            registry.execute("edit", json!({})).await.unwrap()["class"],
            "workspace"
        );
    }

    fn any_class() -> impl Strategy<Value = EffectClass> {
        prop::sample::select(EffectClass::ALL.to_vec())
    }

    proptest! {
        #[test]
        fn at_most_keeps_exactly_the_tools_within_the_ceiling(
            classes in prop::collection::vec(any_class(), 0..12),
            ceiling in any_class(),
        ) {
            let mut registry = ToolRegistry::new();
            for (i, class) in classes.iter().enumerate() {
                registry.register(StubTool::boxed(&format!("tool_{i:02}"), *class));
            }

            let kept = registry.at_most(ceiling);
            prop_assert!(kept.iter().all(|tool| tool.effect_class() <= ceiling));
            let expected = classes.iter().filter(|class| **class <= ceiling).count();
            prop_assert_eq!(kept.len(), expected);
            let kept_names = names(&kept);
            let mut sorted = kept_names.clone();
            sorted.sort();
            prop_assert_eq!(kept_names, sorted);

            let grouped_total: usize = registry.by_class().values().map(Vec::len).sum();
            prop_assert_eq!(grouped_total, classes.len());

            registry.retain_at_most(ceiling);
            prop_assert_eq!(registry.list_tools().len(), expected);
        }
    }

    fn scoped_workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("api/sub")).unwrap();
        std::fs::create_dir_all(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("api/lib.rs"), "pub fn shared() {}").unwrap();
        std::fs::write(dir.path().join("api/sub/deep.rs"), "fn shared() {}").unwrap();
        std::fs::write(dir.path().join("docs/README.md"), "shared docs").unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
        dir
    }

    fn path_scope(writable: &[&str], readable: Option<&[&str]>) -> Option<PathScope> {
        let owned = |values: &[&str]| values.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let readable = readable.map(owned);
        Some(PathScope::new("tester", &owned(writable), readable.as_deref()).unwrap())
    }

    fn denial(err: ToolError) -> ScopeDenial {
        match err {
            ToolError::ScopeDenied(denial) => denial,
            other => panic!("expected ScopeDenied, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn write_file_outside_scope_paths_is_denied() {
        let ws = scoped_workspace();
        let tool = WriteFileTool::scoped(ws.path().to_path_buf(), path_scope(&["api/**"], None));

        let ok = tool
            .execute(json!({ "path": "api/new.rs", "content": "x" }))
            .await
            .unwrap();
        assert_eq!(ok["success"], true);
        assert!(ws.path().join("api/new.rs").exists());

        let err = tool
            .execute(json!({ "path": "docs/new.md", "content": "x" }))
            .await
            .unwrap_err();
        assert!(err
            .to_string()
            .starts_with("Scope denial: identity `tester` may not call `write_file`"));
        let denial = denial(err);
        assert_eq!(denial.identity, "tester");
        assert_eq!(denial.tool, "write_file");
        let expected = DenialReason::PathOutsideScope {
            access: PathAccess::Write,
            path: "docs/new.md".to_string(),
        };
        assert_eq!(denial.reason, expected);
        assert!(!ws.path().join("docs/new.md").exists());

        let escape = tool
            .execute(json!({ "path": "../escape.rs", "content": "x" }))
            .await
            .unwrap_err();
        assert!(matches!(escape, ToolError::PathSecurityViolation { .. }));
    }

    #[tokio::test]
    async fn reads_inside_the_worktree_but_outside_scope_paths_succeed() {
        let ws = scoped_workspace();
        let root = ws.path().to_path_buf();
        let read = ReadFileTool::scoped(root.clone(), path_scope(&["api/**"], None));
        let result = read
            .execute(json!({ "path": "docs/README.md" }))
            .await
            .unwrap();
        assert_eq!(result["total_lines"], 1);

        let list = ListDirTool::scoped(root.clone(), path_scope(&["api/**"], None));
        let listing = list.execute(json!({ "recursive": true })).await.unwrap();
        assert_eq!(listing["count"], 4);

        let search = SearchTool::scoped(root, path_scope(&["api/**"], None));
        let found = search
            .execute(json!({ "pattern": "shared" }))
            .await
            .unwrap();
        assert_eq!(found["count"], 3);
    }

    #[tokio::test]
    async fn read_file_honours_read_paths() {
        let ws = scoped_workspace();
        let scope = path_scope(&["api/**"], Some(&["api/**"]));
        let tool = ReadFileTool::scoped(ws.path().to_path_buf(), scope);
        assert!(tool.execute(json!({ "path": "api/lib.rs" })).await.is_ok());
        let err = tool
            .execute(json!({ "path": "docs/README.md" }))
            .await
            .unwrap_err();
        let expected = DenialReason::PathOutsideScope {
            access: PathAccess::Read,
            path: "docs/README.md".to_string(),
        };
        assert_eq!(denial(err).reason, expected);
    }

    #[tokio::test]
    async fn list_directory_shows_only_readable_files_and_the_directories_leading_to_them() {
        let ws = scoped_workspace();
        let scope = path_scope(&[], Some(&["api/sub/**"]));
        let tool = ListDirTool::scoped(ws.path().to_path_buf(), scope);

        let top = tool.execute(json!({})).await.unwrap();
        let names: Vec<&str> = top["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["api"]);

        let api = tool.execute(json!({ "path": "api" })).await.unwrap();
        let names: Vec<&str> = api["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["sub"]);

        let recursive = tool.execute(json!({ "recursive": true })).await.unwrap();
        let paths: Vec<&str> = recursive["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap())
            .collect();
        assert_eq!(paths, vec!["api/sub/deep.rs"]);

        let filtered = tool
            .execute(json!({ "recursive": true, "pattern": "*.md" }))
            .await
            .unwrap();
        assert_eq!(filtered["count"], 0);
    }

    #[tokio::test]
    async fn search_skips_files_outside_read_paths() {
        let ws = scoped_workspace();
        let scope = path_scope(&[], Some(&["docs/**"]));
        let tool = SearchTool::scoped(ws.path().to_path_buf(), scope);
        let found = tool.execute(json!({ "pattern": "shared" })).await.unwrap();
        assert_eq!(found["count"], 1);
        assert_eq!(found["results"][0]["file"], "docs/README.md");
        let inside_api = tool
            .execute(json!({ "pattern": "shared", "path": "api" }))
            .await
            .unwrap();
        assert_eq!(inside_api["count"], 0);
    }

    fn identity_with(ceiling: EffectClass, tools: &[&str]) -> crate::identity::AgentIdentity {
        let mut identity = crate::identity::example();
        identity.scope.max_effect = ceiling;
        identity.scope.tools = tools.iter().map(|t| t.parse().unwrap()).collect();
        identity
    }

    fn sorted_names(registry: &ToolRegistry) -> Vec<String> {
        let mut names: Vec<String> = registry
            .list_tools()
            .iter()
            .map(|s| s.to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn workspace_ceiling_registry_holds_no_repository_ci_sandbox_or_production_tools() {
        let mut registry = create_tool_registry(Path::new("."));
        registry.register(StubTool::boxed("ci_trigger", EffectClass::Ci));
        registry.register(StubTool::boxed("sandbox_deploy", EffectClass::Sandbox));
        registry.register(StubTool::boxed("prod_rollout", EffectClass::Production));
        let identity = identity_with(EffectClass::Workspace, &["*"]);
        let scoped = registry.scoped_for(&identity);
        assert_eq!(scoped.identity(), Some("rust-implementer"));
        assert_eq!(
            sorted_names(&scoped),
            vec![
                "calculate",
                "echo",
                "git_diff",
                "git_status",
                "list_directory",
                "read_file",
                "search",
                "write_file"
            ]
        );
        for tool in scoped.at_most(EffectClass::Production) {
            assert!(
                tool.effect_class() <= EffectClass::Workspace,
                "{}",
                tool.name()
            );
        }
        let names: Vec<String> = scoped
            .get_definitions()
            .iter()
            .map(|d| d.function.name.clone())
            .collect();
        assert!(!names
            .iter()
            .any(|n| n == "github_pr_status" || n == "ci_trigger" || n == "prod_rollout"));
    }

    #[test]
    fn every_ceiling_enumerates_exactly_the_tools_at_or_below_it() {
        for ceiling in EffectClass::ALL {
            let mut registry = ToolRegistry::new();
            for class in EffectClass::ALL {
                registry.register(StubTool::boxed(&format!("tool_{class}"), class));
            }
            let scoped = registry.scoped_for(&identity_with(ceiling, &["tool_*"]));
            let expected: Vec<String> = EffectClass::ALL
                .iter()
                .filter(|c| **c <= ceiling)
                .map(|c| format!("tool_{c}"))
                .collect();
            let mut expected = expected;
            expected.sort();
            assert_eq!(sorted_names(&scoped), expected, "{ceiling}");
        }
    }

    #[test]
    fn tool_patterns_hide_tools_the_identity_did_not_name() {
        let registry = create_tool_registry(Path::new("."));
        let identity = identity_with(EffectClass::Production, &["read_file", "git_*"]);
        let scoped = registry.scoped_for(&identity);
        assert_eq!(
            sorted_names(&scoped),
            vec!["git_diff", "git_push_branch", "git_status", "read_file"]
        );
        assert!(scoped.get_tool("write_file").is_none());
    }

    #[tokio::test]
    async fn calls_to_tools_outside_scope_are_denied_and_counted() {
        let registry = create_tool_registry(Path::new("."));
        let identity = identity_with(EffectClass::Workspace, &["read_file"]);
        let scoped = registry.scoped_for(&identity);
        assert_eq!(scoped.denial_count(), 0);

        let err = scoped
            .execute("github_pr_status", json!({}))
            .await
            .unwrap_err();
        let first = denial(err);
        assert_eq!(first.identity, "rust-implementer");
        assert_eq!(first.tool, "github_pr_status");
        assert_eq!(first.reason, DenialReason::ToolNotInScope);

        let err = scoped.execute("no_such_tool", json!({})).await.unwrap_err();
        assert_eq!(denial(err).reason, DenialReason::ToolNotInScope);
        assert_eq!(scoped.denial_count(), 2);
        assert_eq!(scoped.denials().len(), 2);
        assert_eq!(scoped.denials()[1].tool, "no_such_tool");
    }

    #[tokio::test]
    async fn unscoped_registry_still_reports_not_found() {
        let registry = create_tool_registry(Path::new("."));
        assert_eq!(registry.identity(), None);
        let err = registry
            .execute("no_such_tool", json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::NotFound { .. }));
        assert_eq!(registry.denial_count(), 0);
    }

    #[tokio::test]
    async fn path_denials_raised_by_tools_are_recorded_on_the_registry() {
        let ws = scoped_workspace();
        let identity = identity_with(EffectClass::Workspace, &["write_file", "read_file"]);
        let registry = create_tool_registry_for(ws.path(), &identity).unwrap();
        assert_eq!(sorted_names(&registry), vec!["read_file", "write_file"]);

        let ok = registry
            .execute(
                "write_file",
                json!({ "path": "api/new.rs", "content": "x" }),
            )
            .await;
        assert!(ok.is_ok());
        let err = registry
            .execute(
                "write_file",
                json!({ "path": "Cargo.toml", "content": "x" }),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ScopeDenied(_)));
        let escape = registry
            .execute("write_file", json!({ "path": "../x", "content": "x" }))
            .await
            .unwrap_err();
        assert!(matches!(escape, ToolError::PathSecurityViolation { .. }));

        let denials = registry.denials();
        assert_eq!(denials.len(), 1);
        assert_eq!(denials[0].tool, "write_file");
        let expected = DenialReason::PathOutsideScope {
            access: PathAccess::Write,
            path: "Cargo.toml".to_string(),
        };
        assert_eq!(denials[0].reason, expected);
        assert_eq!(registry.denial_count(), 1);
    }

    #[test]
    fn scoped_registry_builders_reject_invalid_globs() {
        let mut identity = identity_with(EffectClass::Workspace, &["*"]);
        identity.scope.paths = vec!["[".to_string()];
        assert!(create_tool_registry_for(Path::new("."), &identity).is_err());
        let handle = std::sync::Arc::new(crate::container::ContainerHandle {
            name: "scope-test".to_string(),
            runtime: crate::container::ContainerRuntime::None,
            port: None,
            needs_cleanup: false,
        });
        let err = create_container_tool_registry_for(
            Path::new("."),
            handle,
            CONTAINER_WORKSPACE_DIR,
            &identity,
        );
        assert!(err.is_err());
    }

    #[test]
    fn container_registry_for_a_workspace_identity_keeps_run_command() {
        let handle = std::sync::Arc::new(crate::container::ContainerHandle {
            name: "scope-test".to_string(),
            runtime: crate::container::ContainerRuntime::None,
            port: None,
            needs_cleanup: false,
        });
        let identity = identity_with(EffectClass::Workspace, &["run_command", "read_file"]);
        let registry = create_container_tool_registry_for(
            Path::new("."),
            std::sync::Arc::clone(&handle),
            CONTAINER_WORKSPACE_DIR,
            &identity,
        )
        .unwrap();
        assert_eq!(sorted_names(&registry), vec!["read_file", "run_command"]);

        let read_only = identity_with(EffectClass::None, &["run_command", "read_file"]);
        let registry = create_container_tool_registry_for(
            Path::new("."),
            handle,
            CONTAINER_WORKSPACE_DIR,
            &read_only,
        )
        .unwrap();
        assert_eq!(sorted_names(&registry), vec!["read_file"]);
    }

    proptest! {
        #[test]
        fn a_scoped_registry_is_a_subset_of_the_unscoped_one_and_never_exceeds_the_ceiling(
            classes in prop::collection::vec(any_class(), 0..12),
            ceiling in any_class(),
            allowed in prop::collection::btree_set(0usize..12, 0..12),
        ) {
            let build = || {
                let mut registry = ToolRegistry::new();
                for (i, class) in classes.iter().enumerate() {
                    registry.register(StubTool::boxed(&format!("tool_{i:02}"), *class));
                }
                registry
            };
            let unscoped = sorted_names(&build());
            let patterns: Vec<&str> = allowed.iter().map(|i| if *i % 2 == 0 { "tool_?[02468]" } else { "tool_?[13579]" }).collect();
            let identity = identity_with(ceiling, &patterns);
            let scoped = build().scoped_for(&identity);
            let scoped_names = sorted_names(&scoped);
            prop_assert!(scoped_names.iter().all(|name| unscoped.contains(name)));
            for name in &scoped_names {
                prop_assert!(scoped.effect_class_of(name).unwrap() <= ceiling);
                prop_assert!(identity.allows_tool(name));
            }
            let expected = classes.iter().enumerate().filter(|(i, class)| **class <= ceiling && identity.allows_tool(&format!("tool_{i:02}"))).count();
            prop_assert_eq!(scoped_names.len(), expected);
        }
    }

    #[tokio::test]
    async fn test_echo_tool() {
        let tool = EchoTool::new();
        let args = json!({ "message": "Hello, World!" });
        let result = tool.execute(args).await.unwrap();

        assert_eq!(result["echoed"], "Hello, World!");
        assert!(result["timestamp"].is_string());
    }

    #[tokio::test]
    async fn test_calculator_tool() {
        let tool = CalculatorTool::new();

        let args = json!({
            "operation": "add",
            "a": 5.0,
            "b": 3.0
        });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["result"], 8.0);

        let args = json!({
            "operation": "divide",
            "a": 10.0,
            "b": 0.0
        });
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_tool_registry() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool::new()));
        registry.register(Box::new(CalculatorTool::new()));

        assert_eq!(registry.list_tools().len(), 2);
        assert!(registry.get_tool("echo").is_some());
        assert!(registry.get_tool("calculate").is_some());
        assert!(registry.get_tool("nonexistent").is_none());

        let definitions = registry.get_definitions();
        assert_eq!(definitions.len(), 2);

        let result = registry
            .execute("echo", json!({ "message": "test" }))
            .await
            .unwrap();
        assert_eq!(result["echoed"], "test");
    }

    #[tokio::test]
    async fn test_read_file_tool() {
        let temp_dir = std::env::temp_dir().join("nanna_test_read");
        std::fs::create_dir_all(&temp_dir).unwrap();
        let test_file = temp_dir.join("test.txt");
        std::fs::write(&test_file, "line 1\nline 2\nline 3\nline 4\nline 5").unwrap();

        let tool = ReadFileTool::new(temp_dir.clone());

        let args = json!({ "path": "test.txt" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["total_lines"], 5);
        assert_eq!(result["lines_shown"], 5);

        let args = json!({ "path": "test.txt", "start_line": 2, "end_line": 4 });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["lines_shown"], 3);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_read_file_path_security() {
        let temp_dir = std::env::temp_dir().join("nanna_test_security");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let tool = ReadFileTool::new(temp_dir.clone());

        let args = json!({ "path": "../../../etc/passwd" });
        let result = tool.execute(args).await;
        assert!(result.is_err());
        match result {
            Err(ToolError::PathSecurityViolation { .. }) => {}
            _ => panic!("Expected PathSecurityViolation error"),
        }

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_write_file_tool() {
        let temp_dir = std::env::temp_dir().join("nanna_test_write");
        std::fs::create_dir_all(&temp_dir).unwrap();

        let tool = WriteFileTool::new(temp_dir.clone());

        let args = json!({
            "path": "output.txt",
            "content": "Hello, World!"
        });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["success"], true);
        assert_eq!(result["bytes_written"], 13);

        let content = std::fs::read_to_string(temp_dir.join("output.txt")).unwrap();
        assert_eq!(content, "Hello, World!");

        let args = json!({
            "path": "subdir/nested.txt",
            "content": "Nested file"
        });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["success"], true);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_list_directory_tool() {
        let temp_dir = std::env::temp_dir().join("nanna_test_list");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(temp_dir.join("file1.rs"), "").unwrap();
        std::fs::write(temp_dir.join("file2.txt"), "").unwrap();
        std::fs::create_dir_all(temp_dir.join("subdir")).unwrap();

        let tool = ListDirTool::new(temp_dir.clone());

        let args = json!({});
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 3);

        let args = json!({ "pattern": "*.rs" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 1);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_search_tool() {
        let temp_dir = std::env::temp_dir().join("nanna_test_search");
        std::fs::create_dir_all(&temp_dir).unwrap();
        std::fs::write(
            temp_dir.join("test.rs"),
            "fn main() {\n    println!(\"hello\");\n}",
        )
        .unwrap();
        std::fs::write(temp_dir.join("other.txt"), "no match here").unwrap();

        let tool = SearchTool::new(temp_dir.clone());

        let args = json!({ "pattern": "println" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 1);

        let args = json!({ "pattern": "fn|println" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 2);

        let args = json!({ "pattern": "println", "file_pattern": "*.txt" });
        let result = tool.execute(args).await.unwrap();
        assert_eq!(result["count"], 0);

        std::fs::remove_dir_all(&temp_dir).unwrap();
    }

    #[tokio::test]
    async fn test_git_status_tool() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitStatusTool::new(cwd);

        let args = json!({});
        let result = tool.execute(args).await;

        if let Ok(status) = result {
            assert!(status.get("branch").is_some());
            assert!(status.get("commit").is_some());
            assert!(status.get("is_dirty").is_some());
        }
    }

    #[tokio::test]
    async fn test_git_diff_tool() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitDiffTool::new(cwd);

        let args = json!({});
        let result = tool.execute(args).await;

        if let Ok(diff) = result {
            assert!(diff.get("diff").is_some());
            assert!(diff.get("has_changes").is_some());
        }
    }

    #[tokio::test]
    async fn test_create_container_tool_registry_includes_run_command() {
        use std::sync::Arc;
        let temp_dir = tempfile::tempdir().unwrap();

        let handle = Arc::new(crate::container::ContainerHandle {
            name: "test-container".to_string(),
            runtime: crate::container::ContainerRuntime::None,
            port: None,
            needs_cleanup: false,
        });

        let registry =
            create_container_tool_registry(temp_dir.path(), handle, CONTAINER_WORKSPACE_DIR);
        assert!(registry.get_tool("run_command").is_some());
        assert!(registry.get_tool("read_file").is_some());
        assert!(registry.get_tool("write_file").is_some());
        assert!(registry.get_tool("list_directory").is_some());
        assert!(registry.get_tool("search").is_some());
        assert!(registry.get_tool("git_status").is_some());
        assert!(registry.get_tool("git_diff").is_some());
    }

    // -- PrStatusData unit tests --

    #[test]
    fn test_pr_status_l0_full() {
        let data = PrStatusData {
            pr_number: Some(42),
            issue_number: Some(17),
            pr_status: Some("draft".to_string()),
            review_state: None,
            conflict_count: Some(3),
            conflict_files: vec![
                "src/a.rs".to_string(),
                "src/b.rs".to_string(),
                "src/c.rs".to_string(),
            ],
            ahead: Some(0),
            behind: Some(0),
            additions: Some(66),
            deletions: Some(233),
            ci_status: Some("fail".to_string()),
            ci_failing_checks: vec!["lint".to_string()],
            automerge: false,
            staleness_days: None,
            branch: Some("feature".to_string()),
            head_sha: Some("abc123".to_string()),
            has_upstream: true,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "#42 #17 draft conflicts:3 +66/-233 ci:fail");
    }

    #[test]
    fn test_pr_status_l0_ready_approved() {
        let data = PrStatusData {
            pr_number: Some(42),
            issue_number: Some(17),
            pr_status: Some("ready".to_string()),
            review_state: Some("approved".to_string()),
            conflict_count: None,
            conflict_files: vec![],
            ahead: Some(0),
            behind: Some(0),
            additions: Some(12),
            deletions: Some(5),
            ci_status: Some("pass".to_string()),
            ci_failing_checks: vec![],
            automerge: true,
            staleness_days: Some(2),
            branch: Some("feature".to_string()),
            head_sha: Some("abc123".to_string()),
            has_upstream: true,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "#42 #17 ready approved +12/-5 ci:pass automerge 2d");
    }

    #[test]
    fn test_pr_status_l0_no_upstream() {
        let data = PrStatusData {
            pr_number: None,
            issue_number: None,
            pr_status: None,
            review_state: None,
            conflict_count: None,
            conflict_files: vec![],
            ahead: None,
            behind: None,
            additions: Some(5),
            deletions: Some(2),
            ci_status: None,
            ci_failing_checks: vec![],
            automerge: false,
            staleness_days: None,
            branch: None,
            head_sha: Some("abc123".to_string()),
            has_upstream: false,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "abc123 no-upstream +5/-2");
    }

    #[test]
    fn test_pr_status_l0_behind() {
        let data = PrStatusData {
            pr_number: Some(42),
            issue_number: None,
            pr_status: Some("ready".to_string()),
            review_state: None,
            conflict_count: None,
            conflict_files: vec![],
            ahead: Some(0),
            behind: Some(3),
            additions: Some(66),
            deletions: Some(233),
            ci_status: None,
            ci_failing_checks: vec![],
            automerge: false,
            staleness_days: None,
            branch: Some("feature".to_string()),
            head_sha: Some("abc123".to_string()),
            has_upstream: true,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "#42 ready behind:3 +66/-233");
    }

    #[test]
    fn test_pr_status_l0_changes_requested_with_conflicts() {
        let data = PrStatusData {
            pr_number: Some(42),
            issue_number: Some(17),
            pr_status: Some("ready".to_string()),
            review_state: Some("changes-requested".to_string()),
            conflict_count: Some(1),
            conflict_files: vec!["src/main.rs".to_string()],
            ahead: Some(0),
            behind: Some(0),
            additions: Some(100),
            deletions: Some(50),
            ci_status: Some("pending".to_string()),
            ci_failing_checks: vec![],
            automerge: false,
            staleness_days: None,
            branch: Some("feature".to_string()),
            head_sha: Some("abc123".to_string()),
            has_upstream: true,
            changed_files: vec![],
            github_status: GitHubStatus::Connected,
        };

        let l0 = data.to_l0();
        assert_eq!(
            l0,
            "#42 #17 ready changes-requested conflicts:1 +100/-50 ci:pending"
        );
    }

    #[test]
    fn test_pr_status_l0_merged() {
        let data = PrStatusData {
            pr_number: Some(99),
            pr_status: Some("merged".to_string()),
            additions: Some(0),
            deletions: Some(0),
            has_upstream: true,
            ci_status: Some("pass".to_string()),
            github_status: GitHubStatus::Connected,
            ..Default::default()
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "#99 merged +0/-0 ci:pass");
    }

    #[test]
    fn test_pr_status_l0_minimal() {
        // Minimal: just a commit SHA, no upstream, no diff
        let data = PrStatusData {
            head_sha: Some("def456".to_string()),
            has_upstream: false,
            github_status: GitHubStatus::Connected,
            ..Default::default()
        };

        let l0 = data.to_l0();
        assert_eq!(l0, "def456 no-upstream");
    }

    #[test]
    fn test_pr_status_l1_conflicts() {
        let data = PrStatusData {
            conflict_count: Some(2),
            conflict_files: vec!["src/a.rs".to_string(), "src/b.rs".to_string()],
            ..Default::default()
        };

        let detail = data.to_l1("conflicts").unwrap();
        assert!(detail.contains("Conflicting files (2)"));
        assert!(detail.contains("src/a.rs"));
        assert!(detail.contains("src/b.rs"));
    }

    #[test]
    fn test_pr_status_l1_conflicts_none() {
        let data = PrStatusData::default();
        let detail = data.to_l1("conflicts").unwrap();
        assert_eq!(detail, "No merge conflicts.");
    }

    #[test]
    fn test_pr_status_l1_ci_failing() {
        let data = PrStatusData {
            ci_status: Some("fail".to_string()),
            ci_failing_checks: vec!["lint".to_string(), "test-unit".to_string()],
            ..Default::default()
        };

        let detail = data.to_l1("ci").unwrap();
        assert!(detail.contains("CI status: fail"));
        assert!(detail.contains("lint"));
        assert!(detail.contains("test-unit"));
    }

    #[test]
    fn test_pr_status_l1_ci_passing() {
        let data = PrStatusData {
            ci_status: Some("pass".to_string()),
            ..Default::default()
        };

        let detail = data.to_l1("ci").unwrap();
        assert_eq!(detail, "CI status: pass");
    }

    #[test]
    fn test_pr_status_l1_diff() {
        let data = PrStatusData {
            additions: Some(50),
            deletions: Some(20),
            changed_files: vec!["src/main.rs".to_string(), "Cargo.toml".to_string()],
            ..Default::default()
        };

        let detail = data.to_l1("diff").unwrap();
        assert!(detail.contains("+50/-20"));
        assert!(detail.contains("Changed files (2)"));
        assert!(detail.contains("src/main.rs"));
        assert!(detail.contains("Cargo.toml"));
    }

    #[test]
    fn test_pr_status_l1_sync_no_upstream() {
        let data = PrStatusData {
            has_upstream: false,
            ..Default::default()
        };

        let detail = data.to_l1("sync").unwrap();
        assert_eq!(detail, "No upstream tracking branch configured.");
    }

    #[test]
    fn test_pr_status_l1_sync_with_upstream() {
        let data = PrStatusData {
            has_upstream: true,
            ahead: Some(3),
            behind: Some(1),
            ..Default::default()
        };

        let detail = data.to_l1("sync").unwrap();
        assert_eq!(detail, "Sync: 3 ahead, 1 behind upstream");
    }

    #[test]
    fn test_pr_status_l1_review() {
        let data = PrStatusData {
            review_state: Some("approved".to_string()),
            ..Default::default()
        };

        let detail = data.to_l1("review").unwrap();
        assert_eq!(detail, "Review state: approved");
    }

    #[test]
    fn test_pr_status_l1_automerge() {
        let data = PrStatusData {
            automerge: true,
            ..Default::default()
        };

        let detail = data.to_l1("automerge").unwrap();
        assert_eq!(detail, "Automerge: enabled");

        let data2 = PrStatusData::default();
        let detail2 = data2.to_l1("automerge").unwrap();
        assert_eq!(detail2, "Automerge: disabled");
    }

    #[test]
    fn test_pr_status_l1_staleness() {
        let data = PrStatusData {
            staleness_days: Some(5),
            ..Default::default()
        };

        let detail = data.to_l1("staleness").unwrap();
        assert_eq!(detail, "Last updated 5 days ago");
    }

    #[test]
    fn test_pr_status_l1_unknown_field() {
        let data = PrStatusData::default();
        let result = data.to_l1("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unknown field"));
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_definition() {
        let tool = GitHubPrStatusTool::new(PathBuf::from("/tmp"));
        let def = tool.definition();
        assert_eq!(def.function.name, "github_pr_status");
        assert!(def.function.description.contains("PR status"));

        let params = &def.function.parameters;
        let props = params.properties.as_ref().unwrap();
        assert!(props.contains_key("level"));
        assert!(props.contains_key("field"));
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_name() {
        let tool = GitHubPrStatusTool::new(PathBuf::from("/tmp"));
        assert_eq!(tool.name(), "github_pr_status");
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_l0() {
        // Test the tool executes in the current repo (git-based data only, gh may not be available)
        let cwd = std::env::current_dir().unwrap();
        let tool = GitHubPrStatusTool::new(cwd);

        let args = json!({});
        let result = tool.execute(args).await;

        if let Ok(status) = result {
            assert_eq!(status["level"], "l0");
            assert!(status.get("status").is_some());
            let status_str = status["status"].as_str().unwrap();
            // Should contain at least a SHA or PR number
            assert!(!status_str.is_empty());
        }
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_l1() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitHubPrStatusTool::new(cwd);

        let args = json!({ "level": "l1", "field": "sync" });
        let result = tool.execute(args).await;

        if let Ok(detail) = result {
            assert_eq!(detail["level"], "l1");
            assert_eq!(detail["field"], "sync");
            assert!(detail.get("detail").is_some());
        }
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_l1_missing_field() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitHubPrStatusTool::new(cwd);

        let args = json!({ "level": "l1" });
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_github_pr_status_tool_invalid_level() {
        let cwd = std::env::current_dir().unwrap();
        let tool = GitHubPrStatusTool::new(cwd);

        let args = json!({ "level": "l2" });
        let result = tool.execute(args).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_github_pr_status_tool_in_registry() {
        let cwd = std::env::current_dir().unwrap();
        let registry = create_tool_registry(&cwd);
        assert!(registry.get_tool("github_pr_status").is_some());
    }
    const PROTECTED_EXAMPLES: [(&str, &str); 6] = [
        (".nanna/**", ".nanna/agents/x.toml"),
        ("**/.nanna/**", "crates/api/.nanna/agents/x.toml"),
        (".github/workflows/**", ".github/workflows/ci.yml"),
        (".github/CODEOWNERS", ".github/CODEOWNERS"),
        ("codecov.yml", "codecov.yml"),
        ("windows.toml", "windows.toml"),
    ];

    fn write_capable_tools(registry: &ToolRegistry) -> Vec<String> {
        let mut names: Vec<String> = registry
            .at_most(EffectClass::Production)
            .into_iter()
            .filter(|tool| tool.effect_class() >= EffectClass::Workspace)
            .filter(|tool| {
                let definition = tool.definition();
                let properties = definition.function.parameters.properties;
                properties.is_some_and(|props| props.contains_key("path"))
            })
            .map(|tool| tool.name().to_string())
            .collect();
        names.sort();
        names
    }

    async fn assert_all_protected_writes_refused(registry: &ToolRegistry, root: &Path) {
        let tools = write_capable_tools(registry);
        assert_eq!(tools, vec!["write_file".to_string()]);
        for pattern in crate::protected::PROTECTED_PATTERNS {
            assert!(
                PROTECTED_EXAMPLES.iter().any(|(rule, _)| rule == pattern),
                "{pattern} has no example"
            );
        }
        for tool in &tools {
            for (rule, path) in PROTECTED_EXAMPLES {
                let args = json!({ "path": path, "content": "tampered" });
                let err = registry.execute(tool, args).await.unwrap_err();
                match err {
                    ToolError::ProtectedPath(violation) => {
                        assert_eq!(violation.path, path);
                        assert_eq!(violation.rule, rule);
                    }
                    other => panic!("{tool} on {path}: expected ProtectedPath, got {other:?}"),
                }
                assert!(!root.join(path).exists(), "{tool} wrote {path}");
            }
        }
    }

    #[tokio::test]
    async fn every_write_capable_tool_refuses_every_protected_pattern_unscoped() {
        let dir = tempfile::tempdir().unwrap();
        let registry = create_tool_registry(dir.path());
        assert_all_protected_writes_refused(&registry, dir.path()).await;
        let denials = registry.denials();
        assert_eq!(denials.len(), PROTECTED_EXAMPLES.len());
        assert_eq!(denials[0].identity, crate::scope::UNSCOPED_IDENTITY);
        assert_eq!(denials[0].tool, "write_file");
        assert!(matches!(
            denials[0].reason,
            DenialReason::ProtectedPath { .. }
        ));
    }

    #[tokio::test]
    async fn every_write_capable_tool_refuses_every_protected_pattern_under_a_catch_all_scope() {
        let dir = tempfile::tempdir().unwrap();
        let mut identity = identity_with(EffectClass::Workspace, &["write_file"]);
        identity.scope.paths = vec!["**".to_string()];
        let registry = create_tool_registry_for(dir.path(), &identity).unwrap();
        assert_all_protected_writes_refused(&registry, dir.path()).await;
        let denials = registry.denials();
        assert_eq!(denials.len(), PROTECTED_EXAMPLES.len());
        assert!(denials.iter().all(|d| d.identity == identity.name()));
        let ok = registry
            .execute(
                "write_file",
                json!({ "path": "src/lib.rs", "content": "fine" }),
            )
            .await
            .unwrap();
        assert_eq!(ok["success"], true);
    }

    #[tokio::test]
    async fn write_file_scoped_with_an_explicit_protected_set_refuses_the_config_dir() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("cfg/nanna");
        let protected =
            crate::protected::ProtectedPaths::with_config_dir(dir.path(), Some(&config));
        let tool = WriteFileTool::guarded(dir.path().to_path_buf(), None, protected);
        let err = tool
            .execute(json!({ "path": "cfg/nanna/agents/x.toml", "content": "x" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::ProtectedPath(v) if v.rule == "cfg/nanna/**"));
    }

    struct CountingAuditor {
        calls: std::sync::atomic::AtomicUsize,
    }

    impl CountingAuditor {
        fn new() -> Self {
            Self {
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl crate::action_auditor::ActionAuditor for CountingAuditor {
        fn name(&self) -> &str {
            "counting-block-everything"
        }

        async fn review_action(
            &self,
            _review: &ActionReview,
            _context: &ActionContext<'_>,
        ) -> Result<ActionVerdict, crate::action_auditor::ActionAuditError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(ActionVerdict::block(vec![Reason::new(
                ReasonCode::Other,
                "blocked for the test",
            )]))
        }
    }

    fn subject(max_effect: EffectClass) -> ActionSubject {
        ActionSubject {
            task_id: TaskId("t1".to_string()),
            max_effect,
            window: None,
            repo: "example/repo".to_string(),
            branch: None,
            pr: None,
            environment: None,
            paths: vec![],
        }
    }

    #[tokio::test]
    async fn calls_below_repository_never_reach_the_auditor_and_run_directly() {
        let auditor = Arc::new(CountingAuditor::new());
        let gate = Arc::new(ActionGate::new(
            auditor.clone(),
            crate::action_auditor::ActionAuditLog::in_memory(),
        ));
        let mut registry = ToolRegistry::new();
        registry.register(StubTool::boxed("read", EffectClass::None));
        registry.register(StubTool::boxed("edit", EffectClass::Workspace));
        let registry = registry.with_action_gate(gate, subject(EffectClass::Production));

        for tool in ["read", "edit"] {
            let result = registry.execute(tool, json!({})).await;
            assert!(result.is_ok(), "{tool}: {result:?}");
        }
        assert_eq!(auditor.calls(), 0);
        assert!(registry.action_reviews().is_empty());
    }

    #[tokio::test]
    async fn calls_at_or_above_repository_are_reviewed_exactly_once_and_refused_on_a_block() {
        let auditor = Arc::new(CountingAuditor::new());
        let gate = Arc::new(ActionGate::new(
            auditor.clone(),
            crate::action_auditor::ActionAuditLog::in_memory(),
        ));
        let mut registry = ToolRegistry::new();
        let gated_classes = [
            EffectClass::Repository,
            EffectClass::Ci,
            EffectClass::Sandbox,
            EffectClass::Production,
        ];
        for class in gated_classes {
            registry.register(StubTool::boxed(&format!("tool_{class}"), class));
        }
        let registry = registry.with_action_gate(gate, subject(EffectClass::Production));

        for (i, class) in gated_classes.iter().enumerate() {
            let name = format!("tool_{class}");
            let err = registry.execute(&name, json!({})).await.unwrap_err();
            assert!(
                matches!(err, ToolError::ActionDenied(_)),
                "{class}: {err:?}"
            );
            assert_eq!(auditor.calls(), i + 1);
        }
        let reviews = registry.action_reviews();
        assert_eq!(reviews.len(), gated_classes.len());
        let expected_kinds = [
            crate::auditor::VerdictKind::Block,
            crate::auditor::VerdictKind::Block,
            crate::auditor::VerdictKind::Escalate,
            crate::auditor::VerdictKind::Escalate,
        ];
        for ((review, class), expected) in reviews
            .iter()
            .zip(gated_classes.iter())
            .zip(expected_kinds.iter())
        {
            assert_eq!(&review.review.effect_class, class);
            assert_eq!(review.verdict.kind(), *expected, "{class}");
        }
    }

    #[tokio::test]
    async fn an_effectful_call_with_no_action_gate_attached_is_refused_by_default() {
        let mut registry = ToolRegistry::new();
        registry.register(StubTool::boxed("push", EffectClass::Repository));
        let err = registry.execute("push", json!({})).await.unwrap_err();
        match err {
            ToolError::ActionDenied(ActionDenied::Block { reasons }) => {
                assert!(reasons[0].detail.contains("no action auditor configured"));
            }
            other => panic!("expected ActionDenied::Block, got {other:?}"),
        }
        assert_eq!(registry.action_reviews().len(), 1);
    }

    #[tokio::test]
    async fn the_third_denial_in_a_task_is_upgraded_to_an_escalation() {
        let auditor = Arc::new(CountingAuditor::new());
        let gate = Arc::new(ActionGate::new(
            auditor,
            crate::action_auditor::ActionAuditLog::in_memory(),
        ));
        let mut registry = ToolRegistry::new();
        registry.register(StubTool::boxed("push", EffectClass::Repository));
        let registry = registry.with_action_gate(gate, subject(EffectClass::Production));

        for _ in 0..2 {
            let err = registry.execute("push", json!({})).await.unwrap_err();
            assert!(matches!(
                err,
                ToolError::ActionDenied(ActionDenied::Block { .. })
            ));
        }
        let third = registry.execute("push", json!({})).await.unwrap_err();
        match third {
            ToolError::ActionDenied(ActionDenied::Escalate { reasons }) => {
                assert!(reasons
                    .iter()
                    .any(|r| r.code == crate::auditor::ReasonCode::RepeatedDenials));
            }
            other => panic!("expected ActionDenied::Escalate on the third denial, got {other:?}"),
        }
        let reviews = registry.action_reviews();
        assert_eq!(reviews.len(), 3);
        assert_eq!(
            reviews[2].verdict.kind(),
            crate::auditor::VerdictKind::Escalate,
            "the logged verdict must reflect the escalation, not the auditor's raw block"
        );
    }

    #[tokio::test]
    async fn an_auditor_escalating_directly_is_returned_and_logged_as_an_escalation() {
        struct AlwaysEscalates;

        #[async_trait]
        impl crate::action_auditor::ActionAuditor for AlwaysEscalates {
            fn name(&self) -> &str {
                "always-escalates"
            }

            async fn review_action(
                &self,
                _review: &ActionReview,
                _context: &ActionContext<'_>,
            ) -> Result<ActionVerdict, crate::action_auditor::ActionAuditError> {
                Ok(ActionVerdict::escalate(vec![Reason::new(
                    ReasonCode::Other,
                    "escalated directly by the auditor",
                )]))
            }
        }

        let gate = Arc::new(ActionGate::new(
            Arc::new(AlwaysEscalates),
            crate::action_auditor::ActionAuditLog::in_memory(),
        ));
        let mut registry = ToolRegistry::new();
        registry.register(StubTool::boxed("sandbox_deploy", EffectClass::Sandbox));
        let registry = registry.with_action_gate(gate, subject(EffectClass::Production));

        let err = registry
            .execute("sandbox_deploy", json!({}))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ToolError::ActionDenied(ActionDenied::Escalate { .. })
        ));
        let reviews = registry.action_reviews();
        assert_eq!(reviews.len(), 1);
        assert_eq!(
            reviews[0].verdict.kind(),
            crate::auditor::VerdictKind::Escalate
        );
    }

    #[tokio::test]
    async fn allowed_calls_accumulate_as_prior_actions_for_the_next_review() {
        struct RecordingAuditor {
            seen_prior: Mutex<Vec<Vec<EffectClass>>>,
        }

        #[async_trait]
        impl crate::action_auditor::ActionAuditor for RecordingAuditor {
            fn name(&self) -> &str {
                "recording"
            }

            async fn review_action(
                &self,
                review: &ActionReview,
                _context: &ActionContext<'_>,
            ) -> Result<ActionVerdict, crate::action_auditor::ActionAuditError> {
                self.seen_prior
                    .lock()
                    .unwrap()
                    .push(review.prior_actions.clone());
                Ok(ActionVerdict::Allow)
            }
        }

        let auditor = Arc::new(RecordingAuditor {
            seen_prior: Mutex::new(Vec::new()),
        });
        let gate = Arc::new(ActionGate::new(
            auditor.clone(),
            crate::action_auditor::ActionAuditLog::in_memory(),
        ));
        let mut registry = ToolRegistry::new();
        registry.register(StubTool::boxed("push", EffectClass::Repository));
        registry.register(StubTool::boxed("ci_trigger", EffectClass::Ci));
        let registry = registry.with_action_gate(gate, subject(EffectClass::Production));

        registry.execute("push", json!({})).await.unwrap();
        registry.execute("ci_trigger", json!({})).await.unwrap();

        let seen = auditor.seen_prior.lock().unwrap();
        assert_eq!(seen[0], Vec::<EffectClass>::new());
        assert_eq!(seen[1], vec![EffectClass::Repository]);
    }
}

#[cfg(kani)]
mod kani_proofs {
    use std::collections::HashMap;

    /// Model ToolRegistry::register as a pure HashMap insert.
    ///
    /// The real `register()` calls `HashMap::insert(name, tool)` which
    /// silently overwrites any previous entry with the same key. This
    /// harness verifies that property: after two inserts with the same
    /// key, only the last value survives and the collection size is 1.
    #[kani::proof]
    fn register_overwrites_duplicate_key() {
        let mut map: HashMap<u8, u8> = HashMap::new();

        let key: u8 = kani::any();
        let val1: u8 = kani::any();
        let val2: u8 = kani::any();

        map.insert(key, val1);
        assert_eq!(map.len(), 1);

        // Second insert with the same key silently overwrites
        let old = map.insert(key, val2);
        assert_eq!(old, Some(val1));
        assert_eq!(map.len(), 1);
        assert_eq!(map[&key], val2);
    }

    /// When different keys are used, both entries are preserved.
    #[kani::proof]
    fn register_distinct_keys_preserved() {
        let mut map: HashMap<u8, u8> = HashMap::new();

        let k1: u8 = kani::any();
        let k2: u8 = kani::any();
        kani::assume(k1 != k2);

        let v1: u8 = kani::any();
        let v2: u8 = kani::any();

        map.insert(k1, v1);
        map.insert(k2, v2);

        assert_eq!(map.len(), 2);
        assert_eq!(map[&k1], v1);
        assert_eq!(map[&k2], v2);
    }

    /// After removing a key and re-registering, the new value is present.
    #[kani::proof]
    fn register_after_remove_succeeds() {
        let mut map: HashMap<u8, u8> = HashMap::new();

        let key: u8 = kani::any();
        let v1: u8 = kani::any();
        let v2: u8 = kani::any();

        map.insert(key, v1);
        map.remove(&key);
        assert!(map.is_empty());

        map.insert(key, v2);
        assert_eq!(map.len(), 1);
        assert_eq!(map[&key], v2);
    }
}
