//! Agent tools over the QA checkers: `qa_endpoints` and `qa_browser`.

use super::artifacts::{write_json, QaArtifacts, BROWSER_REPORT_FILE};
use super::browser::{BrowserDriver, BrowserScenario, ScenarioError, ScenarioRunner};
use super::manifest::{HttpProbe, Manifest, ManifestChecker, ManifestError};
use super::summary::QaLedger;
use crate::apprun::{AppInstance, RunningApps};
use crate::tools::{Tool, ToolError, ToolRegistry, ToolResult};
use async_trait::async_trait;
use model::types::{FunctionDefinition, JsonSchema, PropertySchema, SchemaType, ToolDefinition};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

/// Name of the endpoint manifest tool.
pub const QA_ENDPOINTS_TOOL: &str = "qa_endpoints";
/// Name of the browser scenario tool.
pub const QA_BROWSER_TOOL: &str = "qa_browser";
/// Label reported when the repository manifest was used.
pub const MANIFEST_SOURCE_REPO: &str = "CHECKS";
/// Label reported when the manifest was derived from the profile.
pub const MANIFEST_SOURCE_DERIVED: &str = "derived";

/// Errors from running QA.
#[derive(Debug, Error)]
pub enum QaError {
    #[error("no application is running for task {task_id}: call app_start first or pass base_url")]
    NoRunningApp { task_id: String },
    #[error("{path} must be a relative path inside the workspace without '..'")]
    InvalidPath { path: String },
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error(transparent)]
    Scenario(#[from] ScenarioError),
    #[error("could not write QA artefact: {0}")]
    Artifact(#[from] std::io::Error),
}

impl From<QaError> for ToolError {
    fn from(e: QaError) -> Self {
        match e {
            QaError::InvalidPath { .. } | QaError::Scenario(ScenarioError::Json(_)) => {
                ToolError::InvalidArguments {
                    message: e.to_string(),
                }
            }
            other => ToolError::ExecutionFailed {
                message: other.to_string(),
            },
        }
    }
}

/// Everything the QA tools of one task share.
#[derive(Clone)]
pub struct QaContext {
    pub task_id: String,
    /// Where `app_start` registers the running instance.
    pub apps: Arc<RunningApps>,
    /// Host path of the task's worktree; artefacts go under it.
    pub workspace_root: PathBuf,
    /// Host path of the repository manifest, used when it exists.
    pub manifest_path: PathBuf,
    /// Manifest used when the repository has none.
    pub derived: Manifest,
    pub probe: Arc<dyn HttpProbe>,
    pub driver: Arc<dyn BrowserDriver>,
    pub ledger: Arc<QaLedger>,
    /// How long browser steps wait for the page, and how often they poll.
    pub wait: Duration,
    pub poll: Duration,
}

/// `rel` resolved under `root`; absolute paths and `..` are refused.
///
/// The lexical check alone would still let a symlinked ancestor (planted by
/// the task itself, which has ordinary write access to `root`) resolve
/// outside the workspace -- `rel` and every component it joins onto `root`
/// must exist, so the result is canonicalised and re-checked against `root`
/// before it is handed to a file reader.
pub fn workspace_file(root: &Path, rel: &str) -> Result<PathBuf, QaError> {
    let path = Path::new(rel);
    let safe = path
        .components()
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
    if rel.is_empty() || !safe {
        return Err(QaError::InvalidPath {
            path: rel.to_string(),
        });
    }
    let canonical_root = root.canonicalize().map_err(|_| QaError::InvalidPath {
        path: rel.to_string(),
    })?;
    let canonical = root
        .join(path)
        .canonicalize()
        .map_err(|_| QaError::InvalidPath {
            path: rel.to_string(),
        })?;
    if canonical.starts_with(&canonical_root) {
        Ok(canonical)
    } else {
        Err(QaError::InvalidPath {
            path: rel.to_string(),
        })
    }
}

impl QaContext {
    fn instance(&self) -> Result<AppInstance, QaError> {
        self.apps
            .get(&self.task_id)
            .ok_or_else(|| QaError::NoRunningApp {
                task_id: self.task_id.clone(),
            })
    }

    /// The manifest to run and where it came from: the given path, else the
    /// repository manifest, else the derived one.
    pub fn manifest(&self, path: Option<&str>) -> Result<(Manifest, String), QaError> {
        if let Some(rel) = path {
            let file = workspace_file(&self.workspace_root, rel)?;
            return Ok((Manifest::load(&file)?, rel.to_string()));
        }
        if self.manifest_path.is_file() {
            let manifest = Manifest::load(&self.manifest_path)?;
            return Ok((manifest, MANIFEST_SOURCE_REPO.to_string()));
        }
        Ok((self.derived.clone(), MANIFEST_SOURCE_DERIVED.to_string()))
    }

    /// Run the manifest against `base_url` (default: the running app),
    /// write `endpoints-<n>.json` and return the report with its artefact.
    pub fn run_endpoints(
        &self,
        manifest: Option<&str>,
        base_url: Option<&str>,
    ) -> Result<Value, QaError> {
        let (manifest, source) = self.manifest(manifest)?;
        let base_url = match base_url {
            Some(url) => url.to_string(),
            None => self.instance()?.base_url,
        };
        let report = ManifestChecker::new(Arc::clone(&self.probe)).run(&base_url, &manifest);
        let path = QaArtifacts::new(&self.workspace_root).next_endpoint_report()?;
        write_json(&path, &report.to_json())?;
        self.ledger.record_endpoints(&report, &path);
        let mut result = report.to_json();
        result["all_passed"] = Value::Bool(report.all_passed());
        result["manifest"] = Value::String(source);
        result["artifact"] = Value::String(path.display().to_string());
        result["text"] = Value::String(report.render_text());
        Ok(result)
    }

    /// Run `scenario` against `base_url` (default: the running app's
    /// frontend), write `browser-<n>/report.json` next to the screenshots
    /// and return the report with its artefact directory.
    pub fn run_browser(
        &self,
        scenario: &BrowserScenario,
        base_url: Option<&str>,
    ) -> Result<Value, QaError> {
        let frontend_url = match base_url {
            Some(url) => url.to_string(),
            None => self.instance()?.frontend_url,
        };
        let dir = QaArtifacts::new(&self.workspace_root).next_browser_dir()?;
        let runner = ScenarioRunner::new(Arc::clone(&self.driver), &dir);
        let report = runner
            .with_wait(self.wait, self.poll)
            .run(&frontend_url, scenario);
        let path = dir.join(BROWSER_REPORT_FILE);
        write_json(&path, &report.to_json())?;
        self.ledger.record_browser(&report, &path);
        let mut result = report.to_json();
        result["artifact_dir"] = Value::String(dir.display().to_string());
        result["text"] = Value::String(report.render_text());
        Ok(result)
    }

    /// The scenario argument: an object, or a path to a JSON file relative
    /// to the workspace root.
    pub fn scenario(&self, arg: Option<&Value>) -> Result<BrowserScenario, QaError> {
        match arg {
            Some(Value::String(rel)) => {
                let file = workspace_file(&self.workspace_root, rel)?;
                Ok(BrowserScenario::load(&file)?)
            }
            Some(value @ Value::Object(_)) => Ok(BrowserScenario::from_value(value.clone())?),
            _ => Err(QaError::InvalidPath {
                path: "scenario (an object or a file path)".to_string(),
            }),
        }
    }
}

fn string_arg(args: &Value, name: &str) -> ToolResult<Option<String>> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.clone())),
        Some(other) => Err(ToolError::InvalidArguments {
            message: format!("'{name}' must be a non-empty string, got {other}"),
        }),
    }
}

fn property(schema_type: SchemaType, description: &str) -> PropertySchema {
    PropertySchema {
        schema_type,
        description: Some(description.to_string()),
        items: None,
    }
}

fn definition(
    name: &str,
    description: &str,
    props: Vec<(&str, PropertySchema)>,
    required: Option<Vec<String>>,
) -> ToolDefinition {
    let properties: HashMap<String, PropertySchema> =
        props.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    ToolDefinition {
        function: FunctionDefinition {
            name: name.to_string(),
            description: description.to_string(),
            parameters: JsonSchema {
                schema_type: SchemaType::Object,
                properties: Some(properties),
                required,
            },
        },
    }
}

/// `qa_endpoints`: request every path of the endpoint manifest on the
/// running application. Arguments `{ manifest?: "path", base_url?: "…" }`;
/// the manifest defaults to the repository's `CHECKS` (or one derived from
/// the profile) and the base URL to the app started by `app_start`.
pub struct QaEndpointsTool {
    ctx: QaContext,
}

impl QaEndpointsTool {
    pub fn new(ctx: QaContext) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for QaEndpointsTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            QA_ENDPOINTS_TOOL,
            "Request every path of the endpoint manifest (the repository's CHECKS file: one path per line, optional expect=<status> and contains=<text>) on the running application and report each status. Defaults to the app started by app_start. Result: { base_url, manifest, checks: [{ path, url, expected, status, passed, snippet, error }], passed, failed, all_passed, artifact, text }.",
            vec![
                ("manifest", property(SchemaType::String, "Manifest file relative to the workspace root (default: CHECKS, or derived from the profile when absent)")),
                ("base_url", property(SchemaType::String, "Origin to check instead of the running application, e.g. http://127.0.0.1:18000")),
            ],
            None,
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let manifest = string_arg(&args, "manifest")?;
        let base_url = string_arg(&args, "base_url")?;
        let result = self
            .ctx
            .run_endpoints(manifest.as_deref(), base_url.as_deref())?;
        Ok(result)
    }

    fn name(&self) -> &str {
        QA_ENDPOINTS_TOOL
    }
}

/// `qa_browser`: run a scripted scenario in headless Chromium inside the
/// dev container against the running frontend. Arguments
/// `{ scenario: { steps: [...] } | "path", base_url?: "…" }`.
pub struct QaBrowserTool {
    ctx: QaContext,
}

impl QaBrowserTool {
    pub fn new(ctx: QaContext) -> Self {
        Self { ctx }
    }
}

#[async_trait]
impl Tool for QaBrowserTool {
    fn definition(&self) -> ToolDefinition {
        definition(
            QA_BROWSER_TOOL,
            "Run a browser scenario in headless Chromium inside the dev container against the running frontend: steps goto {path}, click {selector}, type {selector, text}, expect_text {selector, text} (waits for the text) and screenshot {name}. The first step must be a goto. Result: { frontend_url, steps: [{ step, passed, detail }], passed, screenshots, console_errors: [{ source, text, url }], artifact_dir, text }.",
            vec![
                ("scenario", property(SchemaType::Object, "{ \"steps\": [{ \"step\": \"goto\", \"path\": \"/\" }, { \"step\": \"expect_text\", \"selector\": \"#greeting\", \"text\": \"Hello\" }, { \"step\": \"screenshot\", \"name\": \"home\" }] }, or the path of such a JSON file relative to the workspace root")),
                ("base_url", property(SchemaType::String, "Origin to drive instead of the running application's frontend")),
            ],
            Some(vec!["scenario".to_string()]),
        )
    }

    async fn execute(&self, args: Value) -> ToolResult<Value> {
        let scenario = self.ctx.scenario(args.get("scenario"))?;
        let base_url = string_arg(&args, "base_url")?;
        let result = self.ctx.run_browser(&scenario, base_url.as_deref())?;
        Ok(result)
    }

    fn name(&self) -> &str {
        QA_BROWSER_TOOL
    }
}

/// Register both QA tools for one task on `registry`.
pub fn register_qa_tools(registry: &mut ToolRegistry, ctx: QaContext) {
    registry.register(Box::new(QaEndpointsTool::new(ctx.clone())));
    registry.register(Box::new(QaBrowserTool::new(ctx)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apprun::PortAllocator;
    use crate::qa::browser::stub::{StubDriver, StubPage};
    use crate::qa::cdp::ConsoleError;
    use crate::qa::manifest::{ProbeError, ProbeResponse};
    use serde_json::json;
    use std::sync::Mutex;
    use tempfile::TempDir;

    struct MapProbe {
        responses: HashMap<String, (u16, String)>,
        calls: Mutex<Vec<String>>,
    }

    impl HttpProbe for MapProbe {
        fn get(&self, url: &str) -> Result<ProbeResponse, ProbeError> {
            self.calls.lock().unwrap().push(url.to_string());
            match self.responses.get(url) {
                Some((status, body)) => Ok(ProbeResponse {
                    status: *status,
                    body: body.clone(),
                }),
                None => Err(ProbeError::Request {
                    url: url.to_string(),
                    detail: "refused".to_string(),
                }),
            }
        }
    }

    fn probe(responses: &[(&str, u16, &str)]) -> Arc<MapProbe> {
        Arc::new(MapProbe {
            responses: responses
                .iter()
                .map(|(u, s, b)| (u.to_string(), (*s, b.to_string())))
                .collect(),
            calls: Mutex::new(Vec::new()),
        })
    }

    struct Fixture {
        ctx: QaContext,
        _workspace: TempDir,
        _leases: TempDir,
        allocator: Arc<PortAllocator>,
    }

    fn fixture(probe: Arc<MapProbe>, pages: Vec<StubPage>) -> Fixture {
        let workspace = TempDir::new().unwrap();
        let leases = TempDir::new().unwrap();
        let allocator = Arc::new(PortAllocator::new(45000..=45001, leases.path()));
        let ctx = QaContext {
            task_id: "qa-task".to_string(),
            apps: Arc::new(RunningApps::new()),
            workspace_root: workspace.path().to_path_buf(),
            manifest_path: workspace.path().join("CHECKS"),
            derived: Manifest::derived("/health", &[]),
            probe,
            driver: StubDriver::new(pages),
            ledger: Arc::new(QaLedger::new()),
            wait: Duration::from_millis(20),
            poll: Duration::from_millis(1),
        };
        Fixture {
            ctx,
            _workspace: workspace,
            _leases: leases,
            allocator,
        }
    }

    fn start_app(f: &Fixture) -> AppInstance {
        let lease = f.allocator.allocate("qa-task").unwrap();
        let instance = AppInstance::local("qa-task", lease.port(), 7, "/tmp/l");
        f.ctx.apps.insert(instance.clone(), lease);
        instance
    }

    fn goto_and_expect() -> Value {
        json!({ "steps": [
            { "step": "goto", "path": "/" },
            { "step": "expect_text", "selector": "#greeting", "text": "Hello" },
            { "step": "screenshot", "name": "home" }
        ] })
    }

    #[tokio::test]
    async fn both_tools_fail_typed_before_app_start() {
        let f = fixture(probe(&[]), vec![]);
        let mut registry = ToolRegistry::new();
        register_qa_tools(&mut registry, f.ctx.clone());
        for name in [QA_ENDPOINTS_TOOL, QA_BROWSER_TOOL] {
            assert!(registry.get_tool(name).is_some(), "{name} missing");
        }
        let err = registry
            .execute(QA_ENDPOINTS_TOOL, json!({}))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::ExecutionFailed { ref message } if message == "no application is running for task qa-task: call app_start first or pass base_url"),
            "{err}"
        );
        let err = registry
            .execute(QA_BROWSER_TOOL, json!({ "scenario": goto_and_expect() }))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::ExecutionFailed { ref message } if message.starts_with("no application is running")),
            "{err}"
        );
        assert!(f.ctx.ledger.snapshot().is_empty());
        assert!(
            !f.ctx.workspace_root.join(".nanna-artifacts").exists(),
            "nothing is written before the app runs"
        );
    }

    #[tokio::test]
    async fn qa_endpoints_uses_the_repo_manifest_against_the_running_app() {
        let probe = probe(&[
            ("http://127.0.0.1:45000/", 200, "<html>"),
            (
                "http://127.0.0.1:45000/health/v1",
                200,
                "{\"status\":\"ok\"}",
            ),
            (
                "http://127.0.0.1:45000/api/v1/greeting",
                500,
                "route broken by FIXTURE_BREAK_ROUTE=1",
            ),
        ]);
        let f = fixture(probe.clone(), vec![]);
        std::fs::write(
            &f.ctx.manifest_path,
            "/\n/health/v1 contains=ok\n/api/v1/greeting\n",
        )
        .unwrap();
        let instance = start_app(&f);
        assert_eq!(instance.port, 45000);
        let tool = QaEndpointsTool::new(f.ctx.clone());
        assert_eq!(tool.name(), "qa_endpoints");
        let def = tool.definition();
        assert_eq!(def.function.name, "qa_endpoints");
        let props = def.function.parameters.properties.unwrap();
        assert!(props.contains_key("manifest") && props.contains_key("base_url"));
        assert_eq!(def.function.parameters.required, None);

        let result = tool.execute(json!({})).await.unwrap();
        assert_eq!(result["base_url"], "http://127.0.0.1:45000");
        assert_eq!(result["manifest"], "CHECKS");
        assert_eq!(result["passed"], 2);
        assert_eq!(result["failed"], 1);
        assert_eq!(result["all_passed"], false);
        assert_eq!(result["checks"][2]["status"], 500);
        assert_eq!(
            result["checks"][2]["snippet"],
            "route broken by FIXTURE_BREAK_ROUTE=1"
        );
        assert!(result["text"]
            .as_str()
            .unwrap()
            .ends_with("2 passed, 1 failed"));
        let artifact = PathBuf::from(result["artifact"].as_str().unwrap());
        assert_eq!(
            artifact,
            f.ctx
                .workspace_root
                .join(".nanna-artifacts/qa/endpoints-1.json")
        );
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&artifact).unwrap()).unwrap();
        assert_eq!(written["checks"].as_array().unwrap().len(), 3);
        assert_eq!(
            written.get("artifact"),
            None,
            "the file holds the bare report"
        );
        let summary = f.ctx.ledger.snapshot();
        assert_eq!(summary.endpoint_runs, 1);
        assert_eq!(summary.endpoint_checks_failed, 1);
        assert_eq!(summary.artifacts, vec![artifact.display().to_string()]);

        let again = tool.execute(Value::Null).await.unwrap();
        assert!(again["artifact"]
            .as_str()
            .unwrap()
            .ends_with("endpoints-2.json"));
        assert_eq!(f.ctx.ledger.snapshot().endpoint_runs, 2);
    }

    #[tokio::test]
    async fn qa_endpoints_takes_a_manifest_path_and_a_base_url_override() {
        let probe = probe(&[("https://sandbox.invalid/ready", 204, "")]);
        let f = fixture(probe.clone(), vec![]);
        std::fs::create_dir_all(f.ctx.workspace_root.join("qa")).unwrap();
        std::fs::write(f.ctx.workspace_root.join("qa/smoke"), "/ready expect=204\n").unwrap();
        let tool = QaEndpointsTool::new(f.ctx.clone());
        let result = tool
            .execute(json!({ "manifest": "qa/smoke", "base_url": "https://sandbox.invalid/" }))
            .await
            .unwrap();
        assert_eq!(result["manifest"], "qa/smoke");
        assert_eq!(result["all_passed"], true);
        assert_eq!(result["checks"][0]["url"], "https://sandbox.invalid/ready");
        assert_eq!(
            probe.calls.lock().unwrap().clone(),
            vec!["https://sandbox.invalid/ready"]
        );
        assert!(f.ctx.apps.is_empty(), "no running app was needed");
    }

    #[tokio::test]
    async fn qa_endpoints_derives_a_manifest_when_the_repo_has_none() {
        let probe = probe(&[
            ("http://127.0.0.1:45000/health", 200, "ok"),
            ("http://127.0.0.1:45000/", 200, ""),
        ]);
        let f = fixture(probe, vec![]);
        start_app(&f);
        let result = QaEndpointsTool::new(f.ctx.clone())
            .execute(json!({}))
            .await
            .unwrap();
        assert_eq!(result["manifest"], "derived");
        assert_eq!(result["passed"], 2);
        assert_eq!(result["all_passed"], true);
    }

    #[tokio::test]
    async fn qa_endpoints_rejects_bad_arguments_and_unreadable_manifests() {
        let f = fixture(probe(&[]), vec![]);
        start_app(&f);
        let tool = QaEndpointsTool::new(f.ctx.clone());
        for (args, kind) in [
            (json!({ "manifest": 3 }), "invalid"),
            (json!({ "base_url": "" }), "invalid"),
            (json!({ "manifest": "../CHECKS" }), "invalid"),
            (json!({ "manifest": "/etc/passwd" }), "invalid"),
            (json!({ "manifest": "" }), "invalid"),
            (json!({ "manifest": "missing" }), "invalid"),
        ] {
            let err = tool.execute(args.clone()).await.unwrap_err();
            let matched = match kind {
                "invalid" => matches!(err, ToolError::InvalidArguments { .. }),
                _ => matches!(err, ToolError::ExecutionFailed { .. }),
            };
            assert!(matched, "{args}: {err}");
        }
        std::fs::write(&f.ctx.manifest_path, "# nothing\n").unwrap();
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(
            matches!(err, ToolError::ExecutionFailed { ref message } if message == "manifest declares no checks"),
            "{err}"
        );
        let file = f.ctx.workspace_root.join(".nanna-artifacts");
        std::fs::write(&file, "in the way").unwrap();
        std::fs::write(&f.ctx.manifest_path, "/\n").unwrap();
        let err = tool.execute(json!({})).await.unwrap_err();
        assert!(
            matches!(err, ToolError::ExecutionFailed { ref message } if message.starts_with("could not write QA artefact")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn qa_browser_runs_a_scenario_and_writes_the_run_directory() {
        let mut page = StubPage::new(&[json!("complete"), json!("Hello from the fixture")]);
        page.errors.push_back(ConsoleError {
            source: "network".to_string(),
            text: "Failed to load resource: 404".to_string(),
            url: Some("http://127.0.0.1:45000/favicon.ico".to_string()),
        });
        let f = fixture(probe(&[]), vec![page]);
        start_app(&f);
        let tool = QaBrowserTool::new(f.ctx.clone());
        assert_eq!(tool.name(), "qa_browser");
        let def = tool.definition();
        assert_eq!(
            def.function.parameters.required,
            Some(vec!["scenario".to_string()])
        );
        assert!(def.function.description.contains("expect_text"));
        let result = tool
            .execute(json!({ "scenario": goto_and_expect() }))
            .await
            .unwrap();
        assert_eq!(result["frontend_url"], "http://127.0.0.1:45000");
        assert_eq!(result["passed"], true, "{}", result["text"]);
        assert_eq!(result["steps"].as_array().unwrap().len(), 3);
        let dir = PathBuf::from(result["artifact_dir"].as_str().unwrap());
        assert_eq!(
            dir,
            f.ctx.workspace_root.join(".nanna-artifacts/qa/browser-1")
        );
        let shot = dir.join("home.png");
        assert_eq!(result["screenshots"], json!([shot.display().to_string()]));
        assert!(shot.is_file());
        assert_eq!(result["console_errors"][0]["source"], "network");
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("report.json")).unwrap())
                .unwrap();
        assert_eq!(written["passed"], true);
        let summary = f.ctx.ledger.snapshot();
        assert_eq!(summary.browser_runs, 1);
        assert_eq!(summary.browser_steps_passed, 3);
        assert_eq!(summary.console_errors, 1);
        assert_eq!(
            summary.artifacts,
            vec![
                dir.join("report.json").display().to_string(),
                shot.display().to_string()
            ]
        );
    }

    #[tokio::test]
    async fn qa_browser_loads_a_scenario_file_and_honours_base_url() {
        let page = StubPage::new(&[json!("complete")]);
        let f = fixture(probe(&[]), vec![page]);
        let file = f.ctx.workspace_root.join("scenario.json");
        std::fs::write(
            &file,
            r#"{ "steps": [{ "step": "goto", "path": "/login" }] }"#,
        )
        .unwrap();
        let result = QaBrowserTool::new(f.ctx.clone())
            .execute(json!({ "scenario": "scenario.json", "base_url": "https://sandbox.invalid" }))
            .await
            .unwrap();
        assert_eq!(result["frontend_url"], "https://sandbox.invalid");
        assert_eq!(
            result["steps"][0]["detail"],
            "loaded https://sandbox.invalid/login"
        );
        assert_eq!(result["passed"], true);
    }

    #[tokio::test]
    async fn qa_browser_rejects_bad_scenarios_and_reports_run_failures() {
        let f = fixture(probe(&[]), vec![]);
        start_app(&f);
        let tool = QaBrowserTool::new(f.ctx.clone());
        for args in [
            json!({}),
            json!({ "scenario": 5 }),
            json!({ "scenario": "../x.json" }),
            json!({ "scenario": { "steps": [] } }),
            json!({ "scenario": { "steps": [{ "step": "fly" }] } }),
            json!({ "scenario": goto_and_expect(), "base_url": 1 }),
        ] {
            let err = tool.execute(args.clone()).await.unwrap_err();
            assert!(
                matches!(
                    err,
                    ToolError::InvalidArguments { .. } | ToolError::ExecutionFailed { .. }
                ),
                "{args}: {err}"
            );
        }
        let err = tool
            .execute(json!({ "scenario": { "steps": [{ "step": "fly" }] } }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }), "{err}");
        let err = tool
            .execute(json!({ "scenario": { "steps": [] } }))
            .await
            .unwrap_err();
        assert!(
            matches!(err, ToolError::ExecutionFailed { ref message } if message == "scenario has no steps"),
            "{err}"
        );
        let err = tool
            .execute(json!({ "scenario": "missing.json" }))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::InvalidArguments { .. }), "{err}");

        let f = fixture(probe(&[]), vec![]);
        start_app(&f);
        let mut ctx = f.ctx.clone();
        ctx.driver = StubDriver::failing("no chromium");
        let result = QaBrowserTool::new(ctx)
            .execute(json!({ "scenario": goto_and_expect() }))
            .await
            .unwrap();
        assert_eq!(result["passed"], false);
        assert!(result["steps"][0]["detail"]
            .as_str()
            .unwrap()
            .starts_with("browser did not start"));
        assert!(f
            .ctx
            .workspace_root
            .join(".nanna-artifacts/qa/browser-1/report.json")
            .is_file());
    }

    #[test]
    fn workspace_file_accepts_only_relative_paths_inside_the_root() {
        let ws = TempDir::new().unwrap();
        let root = ws.path();
        std::fs::write(root.join("CHECKS"), "/health").unwrap();
        std::fs::create_dir(root.join("qa")).unwrap();
        std::fs::write(root.join("qa/smoke"), "[]").unwrap();
        assert_eq!(
            workspace_file(root, "CHECKS").unwrap(),
            root.canonicalize().unwrap().join("CHECKS")
        );
        assert_eq!(
            workspace_file(root, "./qa/smoke").unwrap(),
            root.canonicalize().unwrap().join("qa/smoke")
        );
        for bad in ["", "/abs", "../up", "a/../b", "does_not_exist"] {
            let err = workspace_file(root, bad).unwrap_err();
            assert!(matches!(err, QaError::InvalidPath { .. }), "{bad}: {err}");
        }
    }

    #[test]
    fn workspace_file_reports_a_nonexistent_root_as_an_invalid_path() {
        let missing = Path::new("/definitely/does/not/exist/here");
        let err = workspace_file(missing, "CHECKS").unwrap_err();
        assert!(matches!(err, QaError::InvalidPath { .. }), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn workspace_file_rejects_a_symlink_escaping_the_workspace() {
        let ws = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        std::fs::write(outside.path().join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(outside.path(), ws.path().join("escape")).unwrap();
        let err = workspace_file(ws.path(), "escape/secret").unwrap_err();
        assert!(matches!(err, QaError::InvalidPath { .. }), "{err}");
    }
}
