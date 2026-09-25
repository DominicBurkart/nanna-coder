//! Local QA of the running application: endpoint manifest checks and
//! headless browser scenarios, run from the dev container and recorded as
//! artefacts under the task workspace.

pub mod browser;
pub mod cdp;
pub mod manifest;

pub use browser::{
    click_expression, step_name, text_expression, type_expression, BrowserDriver, BrowserPage,
    BrowserReport, BrowserScenario, CdpPage, ChromiumDriver, ScenarioError, ScenarioRunner, Step,
    StepResult, DEFAULT_BROWSER_TIMEOUT_SECS, DEFAULT_CHROMIUM_BINARY, DEFAULT_POLL, DEFAULT_WAIT,
    READY_STATE_EXPRESSION, WINDOW_SIZE,
};
pub use cdp::{
    console_error_from_event, decode_base64, string_field, BrowserError, CdpSession, CdpTransport,
    ConsoleError, PipeTransport, ProcessSpawner, TransportSpawner, MESSAGE_TERMINATOR,
};
pub use manifest::{
    evaluate, join_url, snippet, Check, CheckResult, ContainerProbe, HttpProbe, Manifest,
    ManifestChecker, ManifestError, ManifestReport, ProbeError, ProbeResponse,
    DEFAULT_EXPECTED_STATUS, DEFAULT_PROBE_TIMEOUT_SECS, SNIPPET_CHARS,
};
