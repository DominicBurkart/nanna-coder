//! Local QA of the running application: endpoint manifest checks and
//! headless browser scenarios, run from the dev container and recorded as
//! artefacts under the task workspace.

pub mod manifest;

pub use manifest::{
    evaluate, join_url, snippet, Check, CheckResult, ContainerProbe, HttpProbe, Manifest,
    ManifestChecker, ManifestError, ManifestReport, ProbeError, ProbeResponse,
    DEFAULT_EXPECTED_STATUS, DEFAULT_PROBE_TIMEOUT_SECS, SNIPPET_CHARS,
};
