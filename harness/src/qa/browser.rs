//! Browser QA: scripted scenarios run in a headless Chromium inside the dev
//! container against the frontend of the running application.
//!
//! # Scenario format
//!
//! A scenario is a JSON object with a `steps` array. Each step is an object
//! whose `step` field names the action:
//!
//! ```json
//! { "steps": [
//!     { "step": "goto", "path": "/" },
//!     { "step": "expect_text", "selector": "#greeting", "text": "Hello" },
//!     { "step": "click", "selector": "button.submit" },
//!     { "step": "type", "selector": "input[name=q]", "text": "hello" },
//!     { "step": "screenshot", "name": "home" }
//! ] }
//! ```
//!
//! `goto` loads `path` on the frontend URL and waits for the document to be
//! complete; `expect_text` waits until the first element matching the CSS
//! `selector` has `text` in its text content, so a wasm frontend that
//! renders after a fetch is given time to do so; `click` and `type` act on
//! the first matching element (`type` sets the value and dispatches `input`
//! and `change`); `screenshot` saves `<name>.png` into the run's artefact
//! directory. The first step must be a `goto`.
//!
//! ```
//! use harness::qa::browser::{BrowserScenario, Step};
//! use serde_json::json;
//!
//! let scenario = BrowserScenario::from_value(json!({ "steps": [
//!     { "step": "goto", "path": "/" },
//!     { "step": "expect_text", "selector": "#greeting", "text": "Hello" },
//!     { "step": "screenshot", "name": "home" }
//! ] })).unwrap();
//! assert_eq!(scenario.steps.len(), 3);
//! assert_eq!(scenario.steps[0], Step::Goto { path: "/".to_string() });
//!
//! let err = BrowserScenario::from_value(json!({ "steps": [
//!     { "step": "screenshot", "name": "home" }
//! ] })).unwrap_err();
//! assert_eq!(err.to_string(), "the first step must be a goto");
//! ```

use super::cdp::{
    decode_base64, string_field, BrowserError, CdpSession, ConsoleError, TransportSpawner,
};
use super::manifest::join_url;
use crate::container::ContainerHandle;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Browser executable inside the dev container.
pub const DEFAULT_CHROMIUM_BINARY: &str = "chromium";
/// Seconds a browser process may live before `timeout` kills it.
pub const DEFAULT_BROWSER_TIMEOUT_SECS: u64 = 120;
/// How long `goto` and `expect_text` wait for the page.
pub const DEFAULT_WAIT: Duration = Duration::from_secs(10);
/// Interval between polls while waiting.
pub const DEFAULT_POLL: Duration = Duration::from_millis(100);
/// Viewport of the headless browser.
pub const WINDOW_SIZE: (u32, u32) = (1280, 800);
/// Longest text quoted in a step detail.
const DETAIL_CHARS: usize = 200;
const MAX_SCREENSHOT_NAME: usize = 64;

/// One action of a scenario.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Step {
    Goto { path: String },
    Click { selector: String },
    Type { selector: String, text: String },
    ExpectText { selector: String, text: String },
    Screenshot { name: String },
}

/// Errors from reading or validating a scenario.
#[derive(Debug, Error)]
pub enum ScenarioError {
    #[error("scenario is not valid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("could not read scenario {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("scenario has no steps")]
    Empty,
    #[error("the first step must be a goto")]
    FirstStepNotGoto,
    #[error("step {index}: {message}")]
    Step { index: usize, message: String },
}

/// The steps of a scenario in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserScenario {
    pub steps: Vec<Step>,
}

impl BrowserScenario {
    /// Parse and validate a scenario object.
    pub fn from_value(value: Value) -> Result<Self, ScenarioError> {
        let scenario: Self = serde_json::from_value(value)?;
        scenario.validate()?;
        Ok(scenario)
    }

    /// Read and validate the scenario file at `path`.
    pub fn load(path: &Path) -> Result<Self, ScenarioError> {
        let text = std::fs::read_to_string(path).map_err(|source| ScenarioError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::from_value(serde_json::from_str(&text)?)
    }

    fn validate(&self) -> Result<(), ScenarioError> {
        let Some(first) = self.steps.first() else {
            return Err(ScenarioError::Empty);
        };
        if !matches!(first, Step::Goto { .. }) {
            return Err(ScenarioError::FirstStepNotGoto);
        }
        for (index, step) in self.steps.iter().enumerate() {
            validate_step(step).map_err(|message| ScenarioError::Step { index, message })?;
        }
        Ok(())
    }
}

fn validate_step(step: &Step) -> Result<(), String> {
    match step {
        Step::Goto { path } if !path.starts_with('/') => {
            Err(format!("goto path must start with '/', got `{path}`"))
        }
        Step::Click { selector } | Step::Type { selector, .. } | Step::ExpectText { selector, .. }
            if selector.trim().is_empty() =>
        {
            Err("selector must not be empty".to_string())
        }
        Step::ExpectText { text, .. } if text.is_empty() => {
            Err("expect_text needs a non-empty text".to_string())
        }
        Step::Screenshot { name } if !valid_screenshot_name(name) => Err(format!(
            "screenshot name must be 1..={MAX_SCREENSHOT_NAME} characters of [A-Za-z0-9_-], got `{name}`"
        )),
        _ => Ok(()),
    }
}

fn valid_screenshot_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SCREENSHOT_NAME
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// JavaScript returning the text content of the first element matching
/// `selector`, or `null` when nothing matches.
pub fn text_expression(selector: &str) -> String {
    let sel = Value::String(selector.to_string());
    format!(
        "(function(){{var e=document.querySelector({sel});return e===null?null:e.textContent;}})()"
    )
}

/// JavaScript clicking the first element matching `selector`; evaluates to
/// whether an element was found.
pub fn click_expression(selector: &str) -> String {
    let sel = Value::String(selector.to_string());
    format!("(function(){{var e=document.querySelector({sel});if(e===null){{return false;}}e.click();return true;}})()")
}

/// JavaScript setting the value of the first element matching `selector`
/// and dispatching `input` and `change`; evaluates to whether an element
/// was found.
pub fn type_expression(selector: &str, text: &str) -> String {
    let sel = Value::String(selector.to_string());
    let val = Value::String(text.to_string());
    format!("(function(){{var e=document.querySelector({sel});if(e===null){{return false;}}e.focus();e.value={val};e.dispatchEvent(new Event('input',{{bubbles:true}}));e.dispatchEvent(new Event('change',{{bubbles:true}}));return true;}})()")
}

/// JavaScript evaluating to the document's ready state.
pub const READY_STATE_EXPRESSION: &str = "document.readyState";

/// A page the runner drives; [`CdpPage`] in production, a stub in tests.
pub trait BrowserPage: Send {
    /// Start loading `url`; readiness is polled through [`BrowserPage::evaluate`].
    fn goto(&mut self, url: &str) -> Result<(), BrowserError>;
    /// Evaluate a JavaScript expression and return its value by value;
    /// `undefined` becomes `Null`.
    fn evaluate(&mut self, expression: &str) -> Result<Value, BrowserError>;
    fn screenshot_png(&mut self) -> Result<Vec<u8>, BrowserError>;
    fn drain_console_errors(&mut self) -> Vec<ConsoleError>;
    /// Ask the browser to shut down; the process is reaped when the page drops.
    fn close(&mut self) -> Result<(), BrowserError>;
}

/// Opens pages; [`ChromiumDriver`] in production, a stub in tests.
pub trait BrowserDriver: Send + Sync {
    fn open(&self) -> Result<Box<dyn BrowserPage>, BrowserError>;
}

/// [`BrowserPage`] over a [`CdpSession`] attached to a page target.
pub struct CdpPage {
    session: CdpSession,
}

impl CdpPage {
    pub fn new(session: CdpSession) -> Self {
        Self { session }
    }
}

impl BrowserPage for CdpPage {
    fn goto(&mut self, url: &str) -> Result<(), BrowserError> {
        let result = self.session.call("Page.navigate", json!({ "url": url }))?;
        if let Some(error) = result.get("errorText").and_then(Value::as_str) {
            return Err(BrowserError::Navigation {
                url: url.to_string(),
                error: error.to_string(),
            });
        }
        Ok(())
    }

    fn evaluate(&mut self, expression: &str) -> Result<Value, BrowserError> {
        let params = json!({ "expression": expression, "returnByValue": true });
        let result = self.session.call("Runtime.evaluate", params)?;
        if let Some(details) = result.get("exceptionDetails") {
            let message = details
                .pointer("/exception/description")
                .or_else(|| details.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("script threw")
                .to_string();
            return Err(BrowserError::Script { message });
        }
        Ok(result
            .pointer("/result/value")
            .cloned()
            .unwrap_or(Value::Null))
    }

    fn screenshot_png(&mut self) -> Result<Vec<u8>, BrowserError> {
        let params = json!({ "format": "png" });
        let result = self.session.call("Page.captureScreenshot", params)?;
        let data = string_field("Page.captureScreenshot", &result, "data")?;
        decode_base64(&data)
    }

    fn drain_console_errors(&mut self) -> Vec<ConsoleError> {
        self.session.drain_console_errors()
    }

    fn close(&mut self) -> Result<(), BrowserError> {
        self.session.call("Browser.close", json!({})).map(|_| ())
    }
}

/// Starts headless Chromium inside the dev container with its DevTools
/// pipe on the exec session's stdin and stdout.
pub struct ChromiumDriver {
    handle: Arc<ContainerHandle>,
    spawner: Arc<dyn TransportSpawner>,
    binary: String,
    timeout_secs: u64,
}

impl ChromiumDriver {
    pub fn new(handle: Arc<ContainerHandle>, spawner: Arc<dyn TransportSpawner>) -> Self {
        Self {
            handle,
            spawner,
            binary: DEFAULT_CHROMIUM_BINARY.to_string(),
            timeout_secs: DEFAULT_BROWSER_TIMEOUT_SECS,
        }
    }

    pub fn with_binary(mut self, binary: &str) -> Self {
        self.binary = binary.to_string();
        self
    }

    pub fn with_timeout(mut self, timeout_secs: u64) -> Self {
        self.timeout_secs = timeout_secs;
        self
    }

    /// The `sh` script that runs the browser with its protocol pipe on
    /// descriptors 3 and 4 redirected from stdin and to stdout.
    pub fn script(&self) -> String {
        let (width, height) = WINDOW_SIZE;
        format!(
            "exec timeout {} {} --headless=new --no-sandbox --disable-gpu --disable-dev-shm-usage --hide-scrollbars --no-first-run --window-size={width},{height} --user-data-dir=/tmp/nanna-chromium-$$ --remote-debugging-pipe about:blank 3<&0 4>&1",
            self.timeout_secs, self.binary
        )
    }

    /// `exec -i <container> sh -c <script>`, the runtime arguments that
    /// keep stdin open for the pipe.
    pub fn argv(&self) -> Vec<String> {
        vec![
            "exec".to_string(),
            "-i".to_string(),
            self.handle.name.clone(),
            "sh".to_string(),
            "-c".to_string(),
            self.script(),
        ]
    }
}

impl BrowserDriver for ChromiumDriver {
    fn open(&self) -> Result<Box<dyn BrowserPage>, BrowserError> {
        let args = self.argv();
        let program = self.handle.runtime.command();
        let transport =
            self.spawner
                .spawn(program, &args)
                .map_err(|source| BrowserError::Spawn {
                    command: format!("{program} {}", args.join(" ")),
                    source,
                })?;
        let mut session = CdpSession::new(transport);
        session.attach_new_page()?;
        Ok(Box::new(CdpPage::new(session)))
    }
}

/// Outcome of one step.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepResult {
    pub step: Step,
    pub passed: bool,
    pub detail: String,
}

/// Everything a scenario run produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserReport {
    pub frontend_url: String,
    /// One entry per executed step; steps after a browser failure are not run.
    pub steps: Vec<StepResult>,
    /// Every step ran and passed. Console errors are evidence, not a verdict.
    pub passed: bool,
    pub screenshots: Vec<PathBuf>,
    pub console_errors: Vec<ConsoleError>,
}

impl BrowserReport {
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).expect("BrowserReport serialises")
    }

    /// One line per step, then the screenshots and console errors.
    pub fn render_text(&self) -> String {
        let mut out = format!("browser scenario against {}\n", self.frontend_url);
        for result in &self.steps {
            let mark = if result.passed { "PASS" } else { "FAIL" };
            let _ = writeln!(out, "{mark} {}: {}", step_name(&result.step), result.detail);
        }
        for shot in &self.screenshots {
            let _ = writeln!(out, "screenshot {}", shot.display());
        }
        for error in &self.console_errors {
            let _ = writeln!(out, "console error [{}] {}", error.source, error.text);
        }
        let verdict = if self.passed { "passed" } else { "failed" };
        let _ = write!(out, "scenario {verdict}");
        out
    }
}

/// The action name of a step as written in the scenario.
pub fn step_name(step: &Step) -> &'static str {
    match step {
        Step::Goto { .. } => "goto",
        Step::Click { .. } => "click",
        Step::Type { .. } => "type",
        Step::ExpectText { .. } => "expect_text",
        Step::Screenshot { .. } => "screenshot",
    }
}

fn quote(text: &str) -> String {
    let short: String = text.chars().take(DETAIL_CHARS).collect();
    format!(
        "`{}`",
        short.split_whitespace().collect::<Vec<_>>().join(" ")
    )
}

/// Runs scenarios through a [`BrowserDriver`], saving screenshots under
/// `screenshot_dir`.
pub struct ScenarioRunner {
    driver: Arc<dyn BrowserDriver>,
    screenshot_dir: PathBuf,
    wait: Duration,
    poll: Duration,
}

impl ScenarioRunner {
    pub fn new(driver: Arc<dyn BrowserDriver>, screenshot_dir: &Path) -> Self {
        Self {
            driver,
            screenshot_dir: screenshot_dir.to_path_buf(),
            wait: DEFAULT_WAIT,
            poll: DEFAULT_POLL,
        }
    }

    /// How long readiness and text expectations are polled, and how often.
    pub fn with_wait(mut self, wait: Duration, poll: Duration) -> Self {
        self.wait = wait;
        self.poll = poll;
        self
    }

    pub fn run(&self, frontend_url: &str, scenario: &BrowserScenario) -> BrowserReport {
        let mut report = BrowserReport {
            frontend_url: frontend_url.to_string(),
            steps: Vec::new(),
            passed: false,
            screenshots: Vec::new(),
            console_errors: Vec::new(),
        };
        let mut page = match self.driver.open() {
            Ok(page) => page,
            Err(e) => {
                report.steps.push(StepResult {
                    step: scenario.steps[0].clone(),
                    passed: false,
                    detail: format!("browser did not start: {e}"),
                });
                return report;
            }
        };
        let mut index = 0;
        let mut fatal_stop = false;
        while index < scenario.steps.len() && !fatal_stop {
            let step = &scenario.steps[index];
            let outcome = self.run_step(page.as_mut(), frontend_url, step, &mut report.screenshots);
            report.console_errors.extend(page.drain_console_errors());
            let (passed, detail, fatal) = match outcome {
                Ok((passed, detail)) => (passed, detail, false),
                Err(e) => (false, e.to_string(), !step_level(&e)),
            };
            report.steps.push(StepResult {
                step: step.clone(),
                passed,
                detail,
            });
            fatal_stop = fatal;
            index += 1;
        }
        if let Err(e) = page.close() {
            tracing::warn!("browser did not close cleanly: {e}");
        }
        report.console_errors.extend(page.drain_console_errors());
        report.passed =
            report.steps.len() == scenario.steps.len() && report.steps.iter().all(|s| s.passed);
        report
    }

    fn run_step(
        &self,
        page: &mut dyn BrowserPage,
        frontend_url: &str,
        step: &Step,
        screenshots: &mut Vec<PathBuf>,
    ) -> Result<(bool, String), BrowserError> {
        match step {
            Step::Goto { path } => {
                let url = join_url(frontend_url, path);
                page.goto(&url)?;
                let ready = self.wait_for(page, READY_STATE_EXPRESSION, |v| v == "complete")?;
                Ok(match ready {
                    Ok(_) => (true, format!("loaded {url}")),
                    Err(state) => (
                        false,
                        format!(
                            "{url} not complete within {:?}: document is {}",
                            self.wait,
                            describe(&state)
                        ),
                    ),
                })
            }
            Step::ExpectText { selector, text } => {
                let expression = text_expression(selector);
                let found = self.wait_for(page, &expression, |v| {
                    v.as_str().is_some_and(|s| s.contains(text))
                })?;
                Ok(match found {
                    Ok(_) => (
                        true,
                        format!("{} contains {}", quote(selector), quote(text)),
                    ),
                    Err(Value::Null) => (false, format!("no element matches {}", quote(selector))),
                    Err(last) => (
                        false,
                        format!(
                            "{} is {}, expected it to contain {}",
                            quote(selector),
                            describe(&last),
                            quote(text)
                        ),
                    ),
                })
            }
            Step::Click { selector } => {
                let found = page.evaluate(&click_expression(selector))?;
                Ok(element_outcome(found, selector, "clicked"))
            }
            Step::Type { selector, text } => {
                let found = page.evaluate(&type_expression(selector, text))?;
                Ok(element_outcome(
                    found,
                    selector,
                    &format!("typed {} into", quote(text)),
                ))
            }
            Step::Screenshot { name } => {
                let bytes = page.screenshot_png()?;
                let path = self.screenshot_dir.join(format!("{name}.png"));
                write_file(&path, &bytes)?;
                screenshots.push(path.clone());
                Ok((true, format!("saved {}", path.display())))
            }
        }
    }

    /// Poll `expression` until `accept` holds or the wait is over; the
    /// error carries the last value seen.
    fn wait_for(
        &self,
        page: &mut dyn BrowserPage,
        expression: &str,
        accept: impl Fn(&Value) -> bool,
    ) -> Result<Result<Value, Value>, BrowserError> {
        let deadline = Instant::now() + self.wait;
        let mut value = page.evaluate(expression)?;
        while !accept(&value) && Instant::now() < deadline {
            std::thread::sleep(self.poll);
            value = page.evaluate(expression)?;
        }
        if accept(&value) {
            Ok(Ok(value))
        } else {
            Ok(Err(value))
        }
    }
}

fn step_level(error: &BrowserError) -> bool {
    matches!(
        error,
        BrowserError::Script { .. } | BrowserError::Navigation { .. }
    )
}

fn element_outcome(found: Value, selector: &str, verb: &str) -> (bool, String) {
    if found == Value::Bool(true) {
        (true, format!("{verb} {}", quote(selector)))
    } else {
        (false, format!("no element matches {}", quote(selector)))
    }
}

fn describe(value: &Value) -> String {
    match value {
        Value::String(s) => quote(s),
        other => other.to_string(),
    }
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), BrowserError> {
    let io = |source| BrowserError::Write {
        path: path.display().to_string(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    std::fs::write(path, bytes).map_err(io)
}

#[cfg(test)]
pub(crate) mod stub {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    /// A page whose `evaluate` answers come from a script of values keyed by
    /// expression prefix, in order.
    pub(crate) struct StubPage {
        pub answers: VecDeque<Value>,
        pub fallback: Value,
        pub errors: VecDeque<ConsoleError>,
        pub calls: Arc<Mutex<Vec<String>>>,
        pub screenshot: Result<Vec<u8>, String>,
        pub navigation_error: Option<String>,
        pub evaluate_error: Option<BrowserError>,
        pub close_error: bool,
    }

    impl StubPage {
        pub(crate) fn new(answers: &[Value]) -> Self {
            Self {
                answers: answers.iter().cloned().collect(),
                fallback: Value::Null,
                errors: VecDeque::new(),
                calls: Arc::new(Mutex::new(Vec::new())),
                screenshot: Ok(b"\x89PNG\r\n\x1a\nstub".to_vec()),
                navigation_error: None,
                evaluate_error: None,
                close_error: false,
            }
        }
    }

    impl BrowserPage for StubPage {
        fn goto(&mut self, url: &str) -> Result<(), BrowserError> {
            self.calls.lock().unwrap().push(format!("goto {url}"));
            match self.navigation_error.take() {
                Some(error) => Err(BrowserError::Navigation {
                    url: url.to_string(),
                    error,
                }),
                None => Ok(()),
            }
        }

        fn evaluate(&mut self, expression: &str) -> Result<Value, BrowserError> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("evaluate {expression}"));
            if let Some(error) = self.evaluate_error.take() {
                return Err(error);
            }
            Ok(self.answers.pop_front().unwrap_or(self.fallback.clone()))
        }

        fn screenshot_png(&mut self) -> Result<Vec<u8>, BrowserError> {
            self.calls.lock().unwrap().push("screenshot".to_string());
            self.screenshot
                .clone()
                .map_err(|message| BrowserError::Protocol {
                    method: "Page.captureScreenshot".to_string(),
                    message,
                })
        }

        fn drain_console_errors(&mut self) -> Vec<ConsoleError> {
            self.errors.drain(..).collect()
        }

        fn close(&mut self) -> Result<(), BrowserError> {
            self.calls.lock().unwrap().push("close".to_string());
            if self.close_error {
                return Err(BrowserError::Protocol {
                    method: "Browser.close".to_string(),
                    message: "already gone".to_string(),
                });
            }
            Ok(())
        }
    }

    /// A driver handing out one prepared page per `open`.
    pub(crate) struct StubDriver {
        pages: Mutex<VecDeque<StubPage>>,
        pub open_error: Option<String>,
    }

    impl StubDriver {
        pub(crate) fn new(pages: Vec<StubPage>) -> Arc<Self> {
            Arc::new(Self {
                pages: Mutex::new(pages.into()),
                open_error: None,
            })
        }

        pub(crate) fn failing(message: &str) -> Arc<Self> {
            Arc::new(Self {
                pages: Mutex::new(VecDeque::new()),
                open_error: Some(message.to_string()),
            })
        }
    }

    impl BrowserDriver for StubDriver {
        fn open(&self) -> Result<Box<dyn BrowserPage>, BrowserError> {
            if let Some(message) = &self.open_error {
                return Err(BrowserError::Spawn {
                    command: "podman exec -i c sh -c chromium".to_string(),
                    source: std::io::Error::other(message.clone()),
                });
            }
            let page = self
                .pages
                .lock()
                .unwrap()
                .pop_front()
                .expect("a page per open");
            Ok(Box::new(page))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::stub::{StubDriver, StubPage};
    use super::*;
    use crate::container::ContainerRuntime;
    use crate::qa::cdp::CdpTransport;
    use std::sync::Mutex;
    use tempfile::TempDir;

    fn steps_of(steps: Value) -> BrowserScenario {
        BrowserScenario::from_value(json!({ "steps": steps })).unwrap()
    }

    fn fast(driver: Arc<dyn BrowserDriver>, dir: &Path) -> ScenarioRunner {
        ScenarioRunner::new(driver, dir)
            .with_wait(Duration::from_millis(30), Duration::from_millis(1))
    }

    #[test]
    fn scenario_parses_every_step_kind_and_round_trips() {
        let scenario = steps_of(json!([
            { "step": "goto", "path": "/" },
            { "step": "click", "selector": "#b" },
            { "step": "type", "selector": "input", "text": "x" },
            { "step": "expect_text", "selector": "#g", "text": "Hello" },
            { "step": "screenshot", "name": "home-1_a" }
        ]));
        assert_eq!(
            scenario.steps[1],
            Step::Click {
                selector: "#b".to_string()
            }
        );
        assert_eq!(
            scenario.steps[2],
            Step::Type {
                selector: "input".to_string(),
                text: "x".to_string()
            }
        );
        assert_eq!(
            scenario.steps[3],
            Step::ExpectText {
                selector: "#g".to_string(),
                text: "Hello".to_string()
            }
        );
        assert_eq!(
            scenario.steps[4],
            Step::Screenshot {
                name: "home-1_a".to_string()
            }
        );
        let json = serde_json::to_value(&scenario).unwrap();
        assert_eq!(json["steps"][0], json!({ "step": "goto", "path": "/" }));
        let back: BrowserScenario = serde_json::from_value(json).unwrap();
        assert_eq!(back, scenario);
        let names: Vec<&str> = scenario.steps.iter().map(step_name).collect();
        assert_eq!(
            names,
            ["goto", "click", "type", "expect_text", "screenshot"]
        );
    }

    #[test]
    fn scenario_rejects_malformed_and_invalid_input() {
        let cases: Vec<(Value, &str)> = vec![
            (json!({ "steps": [] }), "scenario has no steps"),
            (
                json!({ "steps": [{ "step": "click", "selector": "a" }] }),
                "the first step must be a goto",
            ),
            (
                json!({ "steps": [{ "step": "goto", "path": "home" }] }),
                "step 0: goto path must start with '/'",
            ),
            (
                json!({ "steps": [{ "step": "goto", "path": "/" }, { "step": "click", "selector": " " }] }),
                "step 1: selector must not be empty",
            ),
            (
                json!({ "steps": [{ "step": "goto", "path": "/" }, { "step": "type", "selector": "", "text": "x" }] }),
                "step 1: selector must not be empty",
            ),
            (
                json!({ "steps": [{ "step": "goto", "path": "/" }, { "step": "expect_text", "selector": "#g", "text": "" }] }),
                "step 1: expect_text needs a non-empty text",
            ),
            (
                json!({ "steps": [{ "step": "goto", "path": "/" }, { "step": "screenshot", "name": "a/b" }] }),
                "step 1: screenshot name must be",
            ),
            (
                json!({ "steps": [{ "step": "goto", "path": "/" }, { "step": "screenshot", "name": "" }] }),
                "step 1: screenshot name must be",
            ),
            (
                json!({ "steps": [{ "step": "goto", "path": "/" }, { "step": "screenshot", "name": "x".repeat(65) }] }),
                "step 1: screenshot name must be",
            ),
            (
                json!({ "steps": [{ "step": "hover", "selector": "a" }] }),
                "scenario is not valid",
            ),
            (
                json!({ "steps": [{ "step": "goto" }] }),
                "scenario is not valid",
            ),
            (json!([]), "scenario is not valid"),
        ];
        for (value, message) in cases {
            let err = BrowserScenario::from_value(value.clone()).unwrap_err();
            assert!(err.to_string().starts_with(message), "{value}: {err}");
        }
    }

    #[test]
    fn scenario_loads_from_a_file_and_reports_read_errors() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("scenario.json");
        std::fs::write(&path, r#"{ "steps": [{ "step": "goto", "path": "/" }] }"#).unwrap();
        assert_eq!(BrowserScenario::load(&path).unwrap().steps.len(), 1);
        let err = BrowserScenario::load(&dir.path().join("missing.json")).unwrap_err();
        assert!(matches!(err, ScenarioError::Read { .. }), "{err}");
        assert!(err.to_string().starts_with("could not read scenario"));
        std::fs::write(&path, "{").unwrap();
        assert!(matches!(
            BrowserScenario::load(&path).unwrap_err(),
            ScenarioError::Json(_)
        ));
    }

    #[test]
    fn expressions_quote_selectors_and_text_as_json_strings() {
        let text = text_expression("p#greeting");
        assert!(
            text.contains("document.querySelector(\"p#greeting\")"),
            "{text}"
        );
        assert!(text.ends_with("e.textContent;})()"), "{text}");
        let click = click_expression("a[href=\"/x\"]");
        assert!(
            click.contains("querySelector(\"a[href=\\\"/x\\\"]\")"),
            "{click}"
        );
        assert!(click.contains("e.click();return true;"), "{click}");
        let typed = type_expression("input", "it's \"q\"");
        assert!(typed.contains("e.value=\"it's \\\"q\\\"\";"), "{typed}");
        assert!(
            typed.contains("new Event('input',{bubbles:true})"),
            "{typed}"
        );
        assert!(
            typed.contains("new Event('change',{bubbles:true})"),
            "{typed}"
        );
        assert_eq!(READY_STATE_EXPRESSION, "document.readyState");
    }

    #[test]
    fn runner_executes_every_step_kind_and_saves_screenshots() {
        let dir = TempDir::new().unwrap();
        let shots = dir.path().join("browser-1");
        let mut page = StubPage::new(&[
            json!("loading"),
            json!("complete"),
            json!("loading"),
            json!("Hello from the fixture"),
            json!(true),
            json!(true),
            json!("Hello again"),
        ]);
        page.errors.push_back(ConsoleError {
            source: "network".to_string(),
            text: "Failed to load resource: 404".to_string(),
            url: Some("http://app/favicon.ico".to_string()),
        });
        let calls = Arc::clone(&page.calls);
        let driver = StubDriver::new(vec![page]);
        let scenario = steps_of(json!([
            { "step": "goto", "path": "/" },
            { "step": "expect_text", "selector": "#greeting", "text": "fixture" },
            { "step": "click", "selector": "#btn" },
            { "step": "type", "selector": "input", "text": "hi" },
            { "step": "expect_text", "selector": "#echo", "text": "again" },
            { "step": "screenshot", "name": "home" }
        ]));
        let report = fast(driver, &shots).run("http://app:1/", &scenario);
        assert!(report.passed, "{}", report.render_text());
        assert_eq!(report.frontend_url, "http://app:1/");
        assert_eq!(report.steps.len(), 6);
        assert_eq!(report.steps[0].detail, "loaded http://app:1/");
        assert_eq!(report.steps[1].detail, "`#greeting` contains `fixture`");
        assert_eq!(report.steps[2].detail, "clicked `#btn`");
        assert_eq!(report.steps[3].detail, "typed `hi` into `input`");
        assert_eq!(
            report.steps[5].detail,
            format!("saved {}", shots.join("home.png").display())
        );
        assert_eq!(report.screenshots, vec![shots.join("home.png")]);
        assert!(std::fs::read(shots.join("home.png"))
            .unwrap()
            .starts_with(b"\x89PNG"));
        assert_eq!(report.console_errors.len(), 1);
        assert_eq!(report.console_errors[0].source, "network");
        let calls = calls.lock().unwrap().clone();
        assert_eq!(calls[0], "goto http://app:1/");
        assert_eq!(calls[1], "evaluate document.readyState");
        assert_eq!(calls[2], "evaluate document.readyState");
        assert!(calls[3]
            .starts_with("evaluate (function(){var e=document.querySelector(\"#greeting\")"));
        assert_eq!(calls.last().unwrap(), "close");
        assert_eq!(calls.iter().filter(|c| *c == "screenshot").count(), 1);
        let json = report.to_json();
        assert_eq!(json["passed"], true);
        assert_eq!(
            json["steps"][2]["step"],
            json!({ "step": "click", "selector": "#btn" })
        );
        let back: BrowserReport = serde_json::from_value(json).unwrap();
        assert_eq!(back, report);
        let text = report.render_text();
        assert!(
            text.starts_with(
                "browser scenario against http://app:1/\nPASS goto: loaded http://app:1/\n"
            ),
            "{text}"
        );
        assert!(
            text.contains("PASS expect_text: `#greeting` contains `fixture`\n"),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "screenshot {}\n",
                shots.join("home.png").display()
            )),
            "{text}"
        );
        assert!(
            text.contains("console error [network] Failed to load resource: 404\n"),
            "{text}"
        );
        assert!(text.ends_with("scenario passed"), "{text}");
    }

    #[test]
    fn runner_reports_missing_elements_wrong_text_and_slow_pages() {
        let dir = TempDir::new().unwrap();
        let mut page = StubPage::new(&[]);
        page.fallback = json!("interactive");
        let driver = StubDriver::new(vec![page]);
        let scenario = steps_of(json!([
            { "step": "goto", "path": "/slow" },
            { "step": "expect_text", "selector": "#missing", "text": "x" },
            { "step": "click", "selector": "#missing" },
            { "step": "type", "selector": "#missing", "text": "x" }
        ]));
        let runner = ScenarioRunner::new(driver, dir.path())
            .with_wait(Duration::from_millis(5), Duration::ZERO);
        let report = runner.run("http://app", &scenario);
        assert!(!report.passed);
        assert_eq!(report.steps.len(), 4);
        assert!(
            report.steps.iter().all(|s| !s.passed),
            "{}",
            report.render_text()
        );
        assert!(
            report.steps[0]
                .detail
                .starts_with("http://app/slow not complete within"),
            "{}",
            report.steps[0].detail
        );
        assert!(
            report.steps[0]
                .detail
                .ends_with("document is `interactive`"),
            "{}",
            report.steps[0].detail
        );
        assert_eq!(
            report.steps[1].detail,
            "`#missing` is `interactive`, expected it to contain `x`"
        );
        assert_eq!(report.steps[2].detail, "no element matches `#missing`");
        assert_eq!(report.steps[3].detail, "no element matches `#missing`");
        assert!(report.render_text().ends_with("scenario failed"));

        let mut page = StubPage::new(&[json!("complete")]);
        page.fallback = json!("  goodbye \n world  ");
        let driver = StubDriver::new(vec![page]);
        let scenario = steps_of(json!([
            { "step": "goto", "path": "/" },
            { "step": "expect_text", "selector": "#g", "text": "hello" }
        ]));
        let report = fast(driver, dir.path()).run("http://app", &scenario);
        assert_eq!(
            report.steps[1].detail,
            "`#g` is `goodbye world`, expected it to contain `hello`"
        );
        assert!(report.steps[0].passed);
        assert!(!report.passed);

        let page = StubPage::new(&[json!("complete")]);
        let driver = StubDriver::new(vec![page]);
        let scenario = steps_of(json!([
            { "step": "goto", "path": "/" },
            { "step": "expect_text", "selector": "#never", "text": "x" }
        ]));
        let report = fast(driver, dir.path()).run("http://app", &scenario);
        assert_eq!(report.steps[1].detail, "no element matches `#never`");
        assert!(!report.steps[1].passed);
    }

    #[test]
    fn runner_continues_after_step_level_errors_and_stops_on_browser_failures() {
        let dir = TempDir::new().unwrap();
        let mut page = StubPage::new(&[json!("complete"), json!(true)]);
        page.navigation_error = Some("net::ERR_CONNECTION_REFUSED".to_string());
        let driver = StubDriver::new(vec![page]);
        let scenario = steps_of(json!([
            { "step": "goto", "path": "/" },
            { "step": "goto", "path": "/two" },
            { "step": "click", "selector": "#b" }
        ]));
        let report = fast(driver, dir.path()).run("http://app", &scenario);
        assert_eq!(report.steps.len(), 3);
        assert_eq!(
            report.steps[0].detail,
            "navigation to http://app/ failed: net::ERR_CONNECTION_REFUSED"
        );
        assert!(report.steps[1].passed, "{}", report.steps[1].detail);
        assert!(report.steps[2].passed);
        assert!(!report.passed);

        let mut page = StubPage::new(&[json!(true)]);
        page.evaluate_error = Some(BrowserError::Script {
            message: "SyntaxError: '##' is not a valid selector".to_string(),
        });
        let driver = StubDriver::new(vec![page]);
        let scenario = steps_of(json!([
            { "step": "goto", "path": "/" },
            { "step": "click", "selector": "##" }
        ]));
        let report = fast(driver, dir.path()).run("http://app", &scenario);
        assert_eq!(report.steps.len(), 2);
        assert!(
            !report.steps[0].passed,
            "the script error hits the first evaluate: {}",
            report.steps[0].detail
        );
        assert_eq!(
            report.steps[0].detail,
            "script failed: SyntaxError: '##' is not a valid selector"
        );
        assert!(report.steps[1].passed);

        let mut page = StubPage::new(&[json!("complete")]);
        page.screenshot = Err("target closed".to_string());
        page.close_error = true;
        let driver = StubDriver::new(vec![page]);
        let scenario = steps_of(json!([
            { "step": "goto", "path": "/" },
            { "step": "screenshot", "name": "s" },
            { "step": "click", "selector": "#b" }
        ]));
        let report = fast(driver, dir.path()).run("http://app", &scenario);
        assert_eq!(
            report.steps.len(),
            2,
            "steps after a browser failure are not run"
        );
        assert_eq!(
            report.steps[1].detail,
            "Page.captureScreenshot failed: target closed"
        );
        assert!(report.screenshots.is_empty());
        assert!(!report.passed);
    }

    #[test]
    fn runner_reports_a_browser_that_does_not_start_or_a_screenshot_it_cannot_write() {
        let dir = TempDir::new().unwrap();
        let scenario = steps_of(json!([{ "step": "goto", "path": "/" }]));
        let report =
            fast(StubDriver::failing("no chromium"), dir.path()).run("http://app", &scenario);
        assert_eq!(report.steps.len(), 1);
        assert!(
            report.steps[0]
                .detail
                .starts_with("browser did not start: could not spawn"),
            "{}",
            report.steps[0].detail
        );
        assert!(report.steps[0].detail.ends_with("no chromium"));
        assert!(!report.passed);

        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, "x").unwrap();
        let page = StubPage::new(&[json!("complete")]);
        let scenario = steps_of(
            json!([{ "step": "goto", "path": "/" }, { "step": "screenshot", "name": "s" }]),
        );
        let report = fast(StubDriver::new(vec![page]), &file).run("http://app", &scenario);
        assert!(
            report.steps[1].detail.starts_with("could not write"),
            "{}",
            report.steps[1].detail
        );
        assert_eq!(report.steps.len(), 2);
        assert!(!report.passed);
    }

    struct ScriptedSpawner {
        incoming: Vec<Value>,
        seen: Mutex<Vec<(String, Vec<String>)>>,
        fail: bool,
    }

    struct Replay {
        incoming: std::collections::VecDeque<String>,
        sent: Arc<Mutex<Vec<Value>>>,
    }

    impl CdpTransport for Replay {
        fn send(&mut self, message: &str) -> std::io::Result<()> {
            self.sent
                .lock()
                .unwrap()
                .push(serde_json::from_str(message).unwrap());
            Ok(())
        }
        fn receive(&mut self) -> std::io::Result<String> {
            self.incoming
                .pop_front()
                .ok_or_else(|| std::io::Error::other("eof"))
        }
    }

    impl TransportSpawner for ScriptedSpawner {
        fn spawn(&self, program: &str, args: &[String]) -> std::io::Result<Box<dyn CdpTransport>> {
            self.seen
                .lock()
                .unwrap()
                .push((program.to_string(), args.to_vec()));
            if self.fail {
                return Err(std::io::Error::other("podman missing"));
            }
            Ok(Box::new(Replay {
                incoming: self.incoming.iter().map(Value::to_string).collect(),
                sent: Arc::new(Mutex::new(Vec::new())),
            }))
        }
    }

    fn handle() -> Arc<ContainerHandle> {
        Arc::new(ContainerHandle {
            name: "dev-9".to_string(),
            runtime: ContainerRuntime::Podman,
            port: None,
            needs_cleanup: false,
        })
    }

    fn attach_replies() -> Vec<Value> {
        vec![
            json!({ "id": 1, "result": { "targetId": "T" } }),
            json!({ "id": 2, "result": { "sessionId": "S" } }),
            json!({ "id": 3, "result": {} }),
            json!({ "id": 4, "result": {} }),
            json!({ "id": 5, "result": {} }),
        ]
    }

    #[test]
    fn chromium_driver_execs_the_browser_with_its_pipe_on_stdio() {
        let driver = ChromiumDriver::new(
            handle(),
            Arc::new(ScriptedSpawner {
                incoming: vec![],
                seen: Mutex::new(vec![]),
                fail: false,
            }),
        )
        .with_binary("/usr/bin/chromium")
        .with_timeout(45);
        let argv = driver.argv();
        assert_eq!(&argv[..5], ["exec", "-i", "dev-9", "sh", "-c"]);
        let script = &argv[5];
        assert!(
            script.starts_with("exec timeout 45 /usr/bin/chromium --headless=new --no-sandbox"),
            "{script}"
        );
        assert!(script.contains("--window-size=1280,800"), "{script}");
        assert!(
            script.contains("--user-data-dir=/tmp/nanna-chromium-$$"),
            "{script}"
        );
        assert!(
            script.ends_with("--remote-debugging-pipe about:blank 3<&0 4>&1"),
            "{script}"
        );
        assert_eq!(driver.script(), *script);
        let default = ChromiumDriver::new(
            handle(),
            Arc::new(ScriptedSpawner {
                incoming: vec![],
                seen: Mutex::new(vec![]),
                fail: false,
            }),
        );
        assert!(default.script().starts_with("exec timeout 120 chromium "));
    }

    #[test]
    fn chromium_driver_opens_a_page_and_drives_it_over_the_session() {
        let mut incoming = attach_replies();
        incoming.extend([
            json!({ "id": 6, "result": { "frameId": "F" } }),
            json!({ "id": 7, "result": { "frameId": "F", "errorText": "net::ERR_CONNECTION_REFUSED" } }),
            json!({ "method": "Log.entryAdded", "params": { "entry": { "source": "network", "level": "error", "text": "Failed to load resource" } } }),
            json!({ "id": 8, "result": { "result": { "type": "string", "value": "complete" } } }),
            json!({ "id": 9, "result": { "result": { "type": "undefined" } } }),
            json!({ "id": 10, "result": { "result": { "type": "object", "subtype": "error" }, "exceptionDetails": { "text": "Uncaught", "exception": { "description": "SyntaxError: bad selector" } } } }),
            json!({ "id": 11, "result": { "exceptionDetails": { "text": "Uncaught" } } }),
            json!({ "id": 12, "result": { "data": "aGVsbG8=" } }),
            json!({ "id": 13, "result": {} }),
            json!({ "id": 14, "result": {} }),
        ]);
        let spawner = Arc::new(ScriptedSpawner {
            incoming,
            seen: Mutex::new(vec![]),
            fail: false,
        });
        let driver = ChromiumDriver::new(handle(), spawner.clone());
        let mut page = driver.open().unwrap();
        let seen = spawner.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].0, "podman");
        assert_eq!(seen[0].1[..3], ["exec", "-i", "dev-9"]);
        page.goto("http://app/").unwrap();
        let err = page.goto("http://app/").unwrap_err();
        assert_eq!(
            err.to_string(),
            "navigation to http://app/ failed: net::ERR_CONNECTION_REFUSED"
        );
        assert_eq!(
            page.evaluate("document.readyState").unwrap(),
            json!("complete")
        );
        assert_eq!(page.evaluate("undefined").unwrap(), Value::Null);
        let err = page.evaluate("document.querySelector('##')").unwrap_err();
        assert_eq!(err.to_string(), "script failed: SyntaxError: bad selector");
        let err = page.evaluate("throw 1").unwrap_err();
        assert_eq!(err.to_string(), "script failed: Uncaught");
        assert_eq!(page.screenshot_png().unwrap(), b"hello");
        let err = page.screenshot_png().unwrap_err();
        assert!(matches!(err, BrowserError::MissingField { .. }), "{err}");
        let errors = page.drain_console_errors();
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].text, "Failed to load resource");
        page.close().unwrap();
        let err = page.close().unwrap_err();
        assert!(matches!(err, BrowserError::Pipe { .. }), "{err}");
    }

    #[test]
    fn chromium_driver_reports_spawn_and_attach_failures() {
        let spawner = Arc::new(ScriptedSpawner {
            incoming: vec![],
            seen: Mutex::new(vec![]),
            fail: true,
        });
        let err = ChromiumDriver::new(handle(), spawner)
            .open()
            .map(|_| ())
            .unwrap_err();
        assert!(
            err.to_string()
                .starts_with("could not spawn `podman exec -i dev-9 sh -c exec timeout"),
            "{err}"
        );
        assert!(err.to_string().ends_with("podman missing"), "{err}");
        let spawner = Arc::new(ScriptedSpawner {
            incoming: vec![json!({ "id": 1, "error": { "message": "no browser" } })],
            seen: Mutex::new(vec![]),
            fail: false,
        });
        let err = ChromiumDriver::new(handle(), spawner)
            .open()
            .map(|_| ())
            .unwrap_err();
        assert_eq!(err.to_string(), "Target.createTarget failed: no browser");
    }

    #[test]
    fn quote_and_describe_collapse_and_truncate() {
        assert_eq!(quote(" a \n b "), "`a b`");
        assert_eq!(
            quote(&"x".repeat(DETAIL_CHARS + 10)).len(),
            DETAIL_CHARS + 2
        );
        assert_eq!(describe(&json!("s")), "`s`");
        assert_eq!(describe(&json!(3)), "3");
        assert_eq!(describe(&Value::Null), "null");
    }
}
