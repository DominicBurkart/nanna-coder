//! Local QA of the running application: endpoint manifest checks and
//! headless browser scenarios, run from the dev container and recorded as
//! artefacts under the task workspace.

pub mod artifacts;
pub mod browser;
pub mod cdp;
pub mod manifest;
pub mod summary;
pub mod tools;

pub use artifacts::{
    next_index, write_json, QaArtifacts, ARTIFACT_DIR, BROWSER_PREFIX, BROWSER_REPORT_FILE,
    ENDPOINT_PREFIX, QA_DIR,
};
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
    evaluate, join_url, snippet, trunk_asset_roots, Check, CheckResult, ContainerProbe, HttpProbe,
    Manifest, ManifestChecker, ManifestError, ManifestReport, ProbeError, ProbeResponse,
    DEFAULT_EXPECTED_STATUS, DEFAULT_PROBE_TIMEOUT_SECS, SNIPPET_CHARS,
};
pub use summary::{QaLedger, QaSummary};
pub use tools::{
    register_qa_tools, workspace_file, QaBrowserTool, QaContext, QaEndpointsTool, QaError,
    MANIFEST_SOURCE_DERIVED, MANIFEST_SOURCE_REPO, QA_BROWSER_TOOL, QA_ENDPOINTS_TOOL,
};
