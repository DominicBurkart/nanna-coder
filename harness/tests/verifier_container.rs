use harness::container::{detect_runtime, ContainerHandle};
use harness::tools::Tool;
use harness::verify::{CheckOutcome, CheckStatus, CheckTool, ProcessCheckRunner, VerifyConfig};
use std::process::Command;
use std::sync::Arc;

const IMAGE_VAR: &str = "NANNA_VERIFIER_TEST_IMAGE";

struct Cleanup {
    program: &'static str,
    name: String,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = Command::new(self.program)
            .args(["rm", "-f", &self.name])
            .output();
    }
}

#[test]
fn declared_checks_run_in_a_network_less_container_with_exact_exit_codes() {
    let runtime = detect_runtime();
    let program = runtime.command();
    let name = format!("nanna-verifier-it-{}", std::process::id());
    let _cleanup = Cleanup {
        program,
        name: name.clone(),
    };
    let image = std::env::var(IMAGE_VAR).ok().filter(|v| !v.is_empty());
    let Some(image) = image.filter(|_| runtime.is_available()) else {
        eprintln!(
            "skipped: needs a container runtime and {IMAGE_VAR} naming a local image with sh"
        );
        return;
    };
    let started = Command::new(program)
        .args([
            "run",
            "-d",
            "--network=none",
            "--name",
            &name,
            &image,
            "sleep",
            "300",
        ])
        .output()
        .expect("container runtime starts");
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );

    let handle = ContainerHandle {
        name,
        runtime,
        port: None,
        needs_cleanup: false,
    };
    let runner = ProcessCheckRunner::for_container(&handle, "/");
    let config = VerifyConfig::parse(
        "[[check]]\nname = \"ok\"\ncommand = \"echo from-container\"\n\n\
         [[check]]\nname = \"bad\"\ncommand = \"echo oops >&2; exit 7\"\n\n\
         [[check]]\nname = \"slow\"\ncommand = \"exec sleep 60\"\ntimeout_secs = 1\n",
    )
    .unwrap();
    let tool = CheckTool::new(Arc::new(runner), Arc::new(config));

    let runtime_ok = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let mut outcomes = Vec::new();
    for name in ["ok", "bad", "slow"] {
        let value = runtime_ok
            .block_on(tool.execute(serde_json::json!({ "name": name })))
            .unwrap();
        outcomes.push(serde_json::from_value::<CheckOutcome>(value).unwrap());
    }

    let statuses: Vec<_> = outcomes.iter().map(|o| o.status).collect();
    assert_eq!(
        statuses,
        [
            CheckStatus::Passed,
            CheckStatus::Failed,
            CheckStatus::TimedOut
        ]
    );
    assert_eq!(outcomes[0].exit_code, Some(0));
    assert!(outcomes[0].output.contains("from-container"));
    assert_eq!(outcomes[1].exit_code, Some(7));
    assert!(outcomes[1].output.contains("oops"));
}
