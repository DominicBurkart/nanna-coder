use std::time::Duration;

// E2E test configuration
#[allow(dead_code)]
const E2E_MODEL: &str = "qwen3:0.6b";
#[allow(dead_code)]
const E2E_TIMEOUT: Duration = Duration::from_secs(300);
#[allow(dead_code)]
const CONTAINER_STARTUP_WAIT: Duration = Duration::from_secs(30);
#[allow(dead_code)]
const HEALTH_CHECK_TIMEOUT: Duration = Duration::from_secs(60);
