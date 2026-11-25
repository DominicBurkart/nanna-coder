use std::ffi::CString;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub const UID_BASE: u32 = 20000;
pub const UID_SPAN: u32 = 40000;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(900);
pub const DEFAULT_CPU_SECS: u64 = 3600;
pub const DEFAULT_FSIZE_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const OUTPUT_CAP: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSpec {
    pub uid: u32,
    pub cpu_secs: u64,
    pub fsize_bytes: u64,
    pub hidden: Vec<PathBuf>,
    pub chdir: PathBuf,
    pub network: bool,
    pub program: Vec<String>,
}

pub fn parse_run(args: &[String]) -> Result<RunSpec, String> {
    let mut spec = RunSpec {
        uid: 0,
        cpu_secs: DEFAULT_CPU_SECS,
        fsize_bytes: DEFAULT_FSIZE_BYTES,
        hidden: Vec::new(),
        chdir: PathBuf::from("/"),
        network: false,
        program: Vec::new(),
    };
    let mut iter = args.iter();
    while let Some(flag) = iter.next() {
        if flag == "--" {
            spec.program = iter.cloned().collect();
            break;
        }
        let value = iter.next().ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--uid" => spec.uid = value.parse().map_err(|_| format!("bad uid {value}"))?,
            "--cpu-secs" => spec.cpu_secs = value.parse().map_err(|_| "bad --cpu-secs")?,
            "--fsize-bytes" => spec.fsize_bytes = value.parse().map_err(|_| "bad --fsize-bytes")?,
            "--hide" => spec.hidden.push(PathBuf::from(value)),
            "--chdir" => spec.chdir = PathBuf::from(value),
            "--network" => {
                spec.network = match value.as_str() {
                    "true" => true,
                    "false" => false,
                    other => return Err(format!("bad --network {other}")),
                }
            }
            other => return Err(format!("unknown flag {other}")),
        }
    }
    if !(UID_BASE..UID_BASE + UID_SPAN).contains(&spec.uid) {
        return Err(format!(
            "uid {} is outside the sandbox range {UID_BASE}..{}",
            spec.uid,
            UID_BASE + UID_SPAN
        ));
    }
    Ok(spec)
}

fn os_error(what: &str) -> String {
    format!("{what}: {}", std::io::Error::last_os_error())
}

fn drop_to(uid: u32) -> Result<(), String> {
    unsafe {
        if libc::setgroups(0, std::ptr::null()) != 0 {
            return Err(os_error("setgroups"));
        }
        if libc::setresgid(uid, uid, uid) != 0 {
            return Err(os_error("setresgid"));
        }
        if libc::setresuid(uid, uid, uid) != 0 {
            return Err(os_error("setresuid"));
        }
    }
    Ok(())
}

fn limit(resource: libc::__rlimit_resource_t, value: u64) -> Result<(), String> {
    let rl = libc::rlimit {
        rlim_cur: value,
        rlim_max: value,
    };
    if unsafe { libc::setrlimit(resource, &rl) } != 0 {
        return Err(os_error("setrlimit"));
    }
    Ok(())
}

fn hide(path: &Path) -> Result<(), String> {
    if !path.is_dir() {
        return Ok(());
    }
    let target = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
    let src = CString::new("tmpfs").unwrap();
    let opts = CString::new("size=4k,mode=0555").unwrap();
    let rc = unsafe {
        libc::mount(
            src.as_ptr(),
            target.as_ptr(),
            src.as_ptr(),
            libc::MS_NOSUID | libc::MS_NODEV | libc::MS_NOEXEC,
            opts.as_ptr().cast(),
        )
    };
    if rc != 0 {
        return Err(os_error(&format!("hide {}", path.display())));
    }
    Ok(())
}

pub fn enter(spec: &RunSpec) -> Result<(), String> {
    if unsafe { libc::geteuid() } != 0 {
        return Err("the sandbox helper must start as root to drop to its task uid".to_string());
    }
    limit(libc::RLIMIT_CPU, spec.cpu_secs)?;
    limit(libc::RLIMIT_FSIZE, spec.fsize_bytes)?;
    limit(libc::RLIMIT_CORE, 0)?;
    drop_to(spec.uid)?;
    let mut flags =
        libc::CLONE_NEWUSER | libc::CLONE_NEWIPC | libc::CLONE_NEWUTS | libc::CLONE_NEWNS;
    if !spec.network {
        flags |= libc::CLONE_NEWNET;
    }
    if unsafe { libc::unshare(flags) } != 0 {
        return Err(os_error("unshare"));
    }
    let root = CString::new("/").unwrap();
    let none = CString::new("none").unwrap();
    let rc = unsafe {
        libc::mount(
            none.as_ptr(),
            root.as_ptr(),
            std::ptr::null(),
            libc::MS_REC | libc::MS_PRIVATE,
            std::ptr::null(),
        )
    };
    if rc != 0 {
        return Err(os_error("make mounts private"));
    }
    for path in &spec.hidden {
        hide(path)?;
    }
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } != 0 {
        return Err(os_error("no_new_privs"));
    }
    std::env::set_current_dir(&spec.chdir).map_err(|e| format!("chdir: {e}"))?;
    Ok(())
}

fn interfaces() -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string("/proc/net/dev").map_err(|e| e.to_string())?;
    Ok(text
        .lines()
        .skip(2)
        .filter_map(|l| l.split(':').next())
        .map(|n| n.trim().to_string())
        .collect())
}

pub fn selftest() -> Result<(), String> {
    let spec = RunSpec {
        uid: UID_BASE + UID_SPAN - 1,
        cpu_secs: 60,
        fsize_bytes: DEFAULT_FSIZE_BYTES,
        hidden: Vec::new(),
        chdir: PathBuf::from("/"),
        network: false,
        program: Vec::new(),
    };
    enter(&spec)?;
    let nics = interfaces()?;
    if nics.iter().any(|n| n != "lo") {
        return Err(format!("network namespace is not private: {nics:?}"));
    }
    Ok(())
}

pub fn kill_uid(uid: u32) -> Result<(), String> {
    if !(UID_BASE..UID_BASE + UID_SPAN).contains(&uid) {
        return Err(format!("uid {uid} is outside the sandbox range"));
    }
    drop_to(uid)?;
    unsafe { libc::kill(-1, libc::SIGKILL) };
    Ok(())
}

pub fn helper_main(args: &[String]) -> i32 {
    let usage = "usage: nanna-sandbox selftest | kill-uid UID | run [--uid U --hide DIR --chdir DIR --network true|false --cpu-secs N --fsize-bytes N] -- PROGRAM ARGS";
    let Some((verb, rest)) = args.split_first() else {
        eprintln!("{usage}");
        return 2;
    };
    match verb.as_str() {
        "selftest" => match selftest() {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        },
        "kill-uid" => match rest.first().and_then(|u| u.parse().ok()).map(kill_uid) {
            Some(Ok(())) => 0,
            Some(Err(e)) => {
                eprintln!("{e}");
                1
            }
            None => {
                eprintln!("{usage}");
                2
            }
        },
        "run" => {
            let spec = match parse_run(rest) {
                Ok(spec) if !spec.program.is_empty() => spec,
                Ok(_) => {
                    eprintln!("{usage}");
                    return 2;
                }
                Err(e) => {
                    eprintln!("{e}");
                    return 2;
                }
            };
            if let Err(e) = enter(&spec) {
                eprintln!("sandbox refused to start: {e}");
                return 126;
            }
            let err = Command::new(&spec.program[0])
                .args(&spec.program[1..])
                .exec();
            eprintln!("cannot execute {}: {err}", spec.program[0]);
            127
        }
        _ => {
            eprintln!("{usage}");
            2
        }
    }
}

pub fn probe(helper: &Path) -> Result<(), String> {
    let output = Command::new(helper)
        .arg("selftest")
        .env_clear()
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run {}: {e}", helper.display()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

static NEXT_UID: AtomicU32 = AtomicU32::new(0);

pub fn next_uid() -> u32 {
    let n = NEXT_UID.fetch_add(1, Ordering::SeqCst);
    UID_BASE + (n.wrapping_add(std::process::id())) % UID_SPAN
}

#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub helper: PathBuf,
    pub cargo_home: PathBuf,
    pub path: String,
    pub hidden: Vec<PathBuf>,
    pub timeout: Duration,
    pub cpu_secs: u64,
    pub fsize_bytes: u64,
}

impl SandboxConfig {
    pub fn from_environment(env: &crate::exec_capabilities::Environment) -> Option<Self> {
        Self::build(env, env.cargo_home.clone()?)
    }

    pub fn for_stack(env: &crate::exec_capabilities::Environment) -> Option<Self> {
        Self::build(env, env.cargo_home.clone().unwrap_or_default())
    }

    fn build(env: &crate::exec_capabilities::Environment, cargo_home: PathBuf) -> Option<Self> {
        Some(Self {
            helper: env.helper.clone()?,
            cargo_home,
            path: std::env::join_paths(&env.path_dirs)
                .ok()?
                .to_string_lossy()
                .into_owned(),
            hidden: env.hidden.clone(),
            timeout: DEFAULT_TIMEOUT,
            cpu_secs: DEFAULT_CPU_SECS,
            fsize_bytes: DEFAULT_FSIZE_BYTES,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub success: bool,
    pub timed_out: bool,
    pub stdout: String,
    pub stderr: String,
}

fn drain<R: Read + Send + 'static>(mut reader: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if kept.len() < OUTPUT_CAP {
                        let room = OUTPUT_CAP - kept.len();
                        kept.extend_from_slice(&buf[..n.min(room)]);
                    }
                }
            }
        }
        String::from_utf8_lossy(&kept).into_owned()
    })
}

pub fn lock_flags(argv: &[String], had_lock: bool) -> Vec<String> {
    argv.iter()
        .map(|arg| {
            if had_lock && arg == "--offline" {
                "--frozen".to_string()
            } else {
                arg.clone()
            }
        })
        .collect()
}

fn retry_busy<T>(mut attempt: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut retries = 0;
    loop {
        match attempt() {
            Err(error) if error.raw_os_error() == Some(libc::ETXTBSY) && retries < 40 => {
                retries += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            other => return other,
        }
    }
}

fn own_dir(path: &Path, uid: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    chown_to(path, uid)
}

fn chown_to(path: &Path, uid: u32) -> std::io::Result<()> {
    if unsafe { libc::geteuid() } != 0 {
        return Ok(());
    }
    let c = CString::new(path.as_os_str().as_encoded_bytes())?;
    if unsafe { libc::chown(c.as_ptr(), uid, uid) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecProfile {
    Generic,
    Cargo,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecRequest {
    pub argv: Vec<String>,
    pub network: bool,
    pub timeout: Option<Duration>,
    pub env: Vec<(String, String)>,
}

impl ExecRequest {
    pub fn new(argv: Vec<String>) -> Self {
        Self {
            argv,
            network: false,
            timeout: None,
            env: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SandboxedExec {
    config: SandboxConfig,
    workspace: PathBuf,
    profile: ExecProfile,
    allow_network: bool,
}

impl SandboxedExec {
    pub fn new(config: SandboxConfig, workspace: PathBuf, profile: ExecProfile) -> Self {
        Self {
            config,
            workspace,
            profile,
            allow_network: false,
        }
    }

    pub fn with_network_allowed(mut self, allowed: bool) -> Self {
        self.allow_network = allowed;
        self
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    fn network_for(&self, request: &ExecRequest) -> bool {
        request.network && self.allow_network
    }

    pub fn command_line(&self, uid: u32, request: &ExecRequest) -> Vec<String> {
        let mut line = vec![
            "run".to_string(),
            "--uid".to_string(),
            uid.to_string(),
            "--cpu-secs".to_string(),
            self.config.cpu_secs.to_string(),
            "--fsize-bytes".to_string(),
            self.config.fsize_bytes.to_string(),
            "--chdir".to_string(),
            self.workspace.display().to_string(),
        ];
        if self.network_for(request) {
            line.push("--network".to_string());
            line.push("true".to_string());
        }
        for hidden in &self.config.hidden {
            line.push("--hide".to_string());
            line.push(hidden.display().to_string());
        }
        line.push("--".to_string());
        line.extend(request.argv.iter().cloned());
        line
    }

    pub fn environment(&self, scratch: &Path, extra: &[(String, String)]) -> Vec<(String, String)> {
        let mut env = vec![
            ("PATH".to_string(), self.config.path.clone()),
            (
                "HOME".to_string(),
                scratch.join("home").display().to_string(),
            ),
            (
                "TMPDIR".to_string(),
                scratch.join("tmp").display().to_string(),
            ),
            ("LANG".to_string(), "C.UTF-8".to_string()),
        ];
        if self.profile == ExecProfile::Cargo {
            env.extend([
                (
                    "CARGO_HOME".to_string(),
                    self.config.cargo_home.display().to_string(),
                ),
                ("CARGO_NET_OFFLINE".to_string(), "true".to_string()),
                (
                    "CARGO_TARGET_DIR".to_string(),
                    scratch.join("target").display().to_string(),
                ),
                ("CARGO_INCREMENTAL".to_string(), "0".to_string()),
                ("CARGO_TERM_COLOR".to_string(), "never".to_string()),
            ]);
        }
        env.extend(extra.iter().cloned());
        env
    }

    pub fn kill_uid(&self, uid: u32) {
        let _ = retry_busy(|| {
            Command::new(&self.config.helper)
                .args(["kill-uid", &uid.to_string()])
                .env_clear()
                .status()
        });
    }

    pub fn run(&self, request: &ExecRequest) -> std::io::Result<Outcome> {
        let uid = next_uid();
        let scratch = std::env::temp_dir().join(format!("nanna-exec-{uid}-{}", std::process::id()));
        let result = self.run_leased(uid, &scratch, request);
        let _ = std::fs::remove_dir_all(&scratch);
        result
    }

    pub fn run_leased(
        &self,
        uid: u32,
        scratch: &Path,
        request: &ExecRequest,
    ) -> std::io::Result<Outcome> {
        let mut request = request.clone();
        let mut placeholder = None;
        if self.profile == ExecProfile::Cargo {
            let lock = self.workspace.join("Cargo.lock");
            let had_lock = lock.exists();
            request.argv = lock_flags(&request.argv, had_lock);
            if !had_lock {
                std::fs::write(&lock, b"")?;
                if let Err(error) = chown_to(&lock, uid) {
                    let _ = std::fs::remove_file(&lock);
                    return Err(error);
                }
                placeholder = Some(lock);
            }
        }
        let result = self.run_in(uid, scratch, &request);
        if let Some(lock) = placeholder {
            let _ = std::fs::remove_file(lock);
        }
        self.kill_uid(uid);
        result
    }

    fn run_in(&self, uid: u32, scratch: &Path, request: &ExecRequest) -> std::io::Result<Outcome> {
        for dir in [
            scratch.to_path_buf(),
            scratch.join("home"),
            scratch.join("tmp"),
            scratch.join("target"),
        ] {
            own_dir(&dir, uid)?;
        }
        let mut command = Command::new(&self.config.helper);
        command
            .args(self.command_line(uid, request))
            .env_clear()
            .envs(self.environment(scratch, &request.env))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = retry_busy(|| command.spawn())?;
        let out = drain(child.stdout.take().expect("piped stdout"));
        let err = drain(child.stderr.take().expect("piped stderr"));
        let deadline = Instant::now() + request.timeout.unwrap_or(self.config.timeout);
        let mut timed_out = false;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                unsafe { libc::killpg(child.id() as i32, libc::SIGKILL) };
                self.kill_uid(uid);
                break child.wait()?;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Ok(Outcome {
            success: status.success() && !timed_out,
            timed_out,
            stdout: out.join().unwrap_or_default(),
            stderr: err.join().unwrap_or_default(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct SandboxedCargo {
    exec: SandboxedExec,
}

impl SandboxedCargo {
    pub fn new(config: SandboxConfig, workspace: PathBuf) -> Self {
        Self {
            exec: SandboxedExec::new(config, workspace, ExecProfile::Cargo),
        }
    }

    pub fn command_line(&self, uid: u32, argv: &[String]) -> Vec<String> {
        self.exec
            .command_line(uid, &ExecRequest::new(argv.to_vec()))
    }

    pub fn environment(&self, target: &Path) -> Vec<(String, String)> {
        self.exec.environment(target, &[])
    }

    pub fn run(&self, argv: &[String]) -> std::io::Result<Outcome> {
        self.exec.run(&ExecRequest::new(argv.to_vec()))
    }

    pub fn run_json(&self, argv: &[String]) -> std::io::Result<Value> {
        let outcome = self.run(argv)?;
        Ok(json!({
            "stdout": outcome.stdout,
            "stderr": outcome.stderr,
            "success": outcome.success,
            "timed_out": outcome.timed_out,
            "command": argv.join(" "),
        }))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn strings(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn run_arguments_parse_into_a_spec() {
        let spec = parse_run(&strings(&[
            "--uid",
            "20001",
            "--hide",
            "/repos",
            "--hide",
            "/etc/nanna",
            "--chdir",
            "/w",
            "--",
            "cargo",
            "check",
        ]))
        .unwrap();
        assert_eq!(spec.uid, 20001);
        assert_eq!(
            spec.hidden,
            vec![PathBuf::from("/repos"), PathBuf::from("/etc/nanna")]
        );
        assert_eq!(spec.chdir, PathBuf::from("/w"));
        assert_eq!(spec.program, strings(&["cargo", "check"]));
    }

    #[test]
    fn uids_outside_the_sandbox_range_are_refused() {
        for uid in ["0", "1000", "19999", "60000", "70000"] {
            let err = parse_run(&strings(&["--uid", uid, "--", "x"])).unwrap_err();
            assert!(err.contains("outside the sandbox range"), "{uid}: {err}");
        }
        assert!(kill_uid(0).is_err());
        assert!(kill_uid(1000).is_err());
    }

    #[test]
    fn malformed_arguments_are_refused() {
        assert!(parse_run(&strings(&["--uid"])).is_err());
        assert!(parse_run(&strings(&["--uid", "abc", "--", "x"])).is_err());
        assert!(parse_run(&strings(&["--bogus", "1", "--", "x"])).is_err());
    }

    #[test]
    fn allocated_uids_stay_in_range_and_differ() {
        let a = next_uid();
        let b = next_uid();
        assert_ne!(a, b);
        for uid in [a, b] {
            assert!((UID_BASE..UID_BASE + UID_SPAN).contains(&uid));
        }
    }

    fn config() -> SandboxConfig {
        SandboxConfig {
            helper: PathBuf::from("/bin/nanna-sandbox"),
            cargo_home: PathBuf::from("/cargo"),
            path: "/bin".to_string(),
            hidden: vec![PathBuf::from("/repos")],
            timeout: Duration::from_secs(5),
            cpu_secs: 10,
            fsize_bytes: 1024,
        }
    }

    #[test]
    fn the_cargo_environment_is_a_scrubbed_allow_list() {
        let cargo = SandboxedCargo::new(config(), PathBuf::from("/tmp/nanna-task-1"));
        let env = cargo.environment(Path::new("/tmp/scratch"));
        let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        for needed in [
            "PATH",
            "CARGO_HOME",
            "CARGO_NET_OFFLINE",
            "CARGO_TARGET_DIR",
            "HOME",
            "TMPDIR",
        ] {
            assert!(names.contains(&needed), "{needed}");
        }
        for forbidden in [
            "GITHUB_TOKEN",
            "NANNA_CONFIG_DIR",
            "SSH_AUTH_SOCK",
            "RUSTC_WRAPPER",
        ] {
            assert!(!names.contains(&forbidden), "{forbidden}");
        }
        let get = |k: &str| env.iter().find(|(n, _)| n == k).unwrap().1.clone();
        assert_eq!(get("CARGO_NET_OFFLINE"), "true");
        assert_eq!(get("CARGO_HOME"), "/cargo");
        assert!(get("CARGO_TARGET_DIR").starts_with("/tmp/scratch/"));
    }

    #[test]
    fn the_command_line_carries_uid_limits_hides_and_the_program() {
        let cargo = SandboxedCargo::new(config(), PathBuf::from("/tmp/nanna-task-1"));
        let line = cargo.command_line(20005, &strings(&["cargo", "check", "--frozen"]));
        assert_eq!(line[0], "run");
        let spec = parse_run(&line[1..]).unwrap();
        assert_eq!(spec.uid, 20005);
        assert_eq!(spec.cpu_secs, 10);
        assert_eq!(spec.fsize_bytes, 1024);
        assert_eq!(spec.hidden, vec![PathBuf::from("/repos")]);
        assert_eq!(spec.chdir, PathBuf::from("/tmp/nanna-task-1"));
        assert_eq!(spec.program, strings(&["cargo", "check", "--frozen"]));
    }

    #[test]
    fn a_missing_helper_fails_the_probe_and_the_run() {
        let missing = Path::new("/nonexistent/nanna-sandbox");
        assert!(probe(missing).is_err());
        let mut cfg = config();
        cfg.helper = missing.to_path_buf();
        let cargo = SandboxedCargo::new(cfg, PathBuf::from("/tmp"));
        assert!(cargo.run(&strings(&["true"])).is_err());
    }

    #[test]
    fn helper_main_rejects_bad_usage() {
        assert_eq!(helper_main(&[]), 2);
        assert_eq!(helper_main(&strings(&["bogus"])), 2);
        assert_eq!(helper_main(&strings(&["kill-uid"])), 2);
        assert_eq!(helper_main(&strings(&["run", "--uid", "20000"])), 2);
        assert_eq!(helper_main(&strings(&["run", "--uid", "5", "--", "x"])), 2);
    }

    #[test]
    fn a_committed_lockfile_freezes_cargo_and_a_missing_one_stays_offline() {
        let argv = strings(&["cargo", "check", "--offline", "--package", "x"]);
        assert_eq!(
            lock_flags(&argv, true),
            strings(&["cargo", "check", "--frozen", "--package", "x"])
        );
        assert_eq!(lock_flags(&argv, false), argv);
    }

    #[test]
    fn output_is_capped() {
        let big = vec![b'a'; OUTPUT_CAP * 2];
        let kept = drain(std::io::Cursor::new(big)).join().unwrap();
        assert_eq!(kept.len(), OUTPUT_CAP);
    }

    pub(crate) fn fake_helper(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("nanna-sandbox");
        let script = r#"#!/bin/sh
dir=$(dirname "$0")
case "$1" in
kill-uid) echo "kill $2" >> "$dir/log"; exit 0;;
run)
  shift
  while [ $# -gt 0 ]; do
    case "$1" in
      --uid) echo "run $2" >> "$dir/log"; shift 2;;
      --network) echo "network $2" >> "$dir/log"; shift 2;;
      --chdir) cd "$2" || exit 126; shift 2;;
      --) shift; break;;
      *) shift 2;;
    esac
  done
  exec "$@";;
esac
exit 2
"#;
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    pub(crate) fn fake_log(dir: &Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    pub(crate) fn fake_config(helper: PathBuf) -> SandboxConfig {
        SandboxConfig {
            helper,
            cargo_home: PathBuf::from("/cargo"),
            path: "/usr/bin:/bin".to_string(),
            hidden: Vec::new(),
            timeout: Duration::from_secs(20),
            cpu_secs: 60,
            fsize_bytes: 1 << 30,
        }
    }

    fn request(parts: &[&str]) -> ExecRequest {
        ExecRequest::new(strings(parts))
    }

    #[test]
    fn a_busy_executable_is_retried_and_other_errors_are_not() {
        let mut calls = 0;
        let result = retry_busy(|| {
            calls += 1;
            if calls < 3 {
                Err(std::io::Error::from_raw_os_error(libc::ETXTBSY))
            } else {
                Ok(calls)
            }
        });
        assert_eq!(result.unwrap(), 3);
        let mut other_calls = 0;
        let result: std::io::Result<()> = retry_busy(|| {
            other_calls += 1;
            Err(std::io::Error::from_raw_os_error(libc::ENOENT))
        });
        assert!(result.is_err());
        assert_eq!(other_calls, 1);
    }

    #[test]
    fn network_defaults_off_in_run_arguments() {
        let spec = parse_run(&strings(&["--uid", "20001", "--", "x"])).unwrap();
        assert!(!spec.network);
        let spec = parse_run(&strings(&[
            "--uid",
            "20001",
            "--network",
            "true",
            "--",
            "x",
        ]))
        .unwrap();
        assert!(spec.network);
        assert!(parse_run(&strings(&[
            "--uid",
            "20001",
            "--network",
            "maybe",
            "--",
            "x"
        ]))
        .is_err());
    }

    #[test]
    fn the_generic_environment_carries_no_cargo_variables() {
        let exec = SandboxedExec::new(config(), PathBuf::from("/w"), ExecProfile::Generic);
        let env = exec.environment(
            Path::new("/tmp/scratch"),
            &[("GOFLAGS".to_string(), "-mod=vendor".to_string())],
        );
        let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        for needed in ["PATH", "HOME", "TMPDIR", "LANG", "GOFLAGS"] {
            assert!(names.contains(&needed), "{needed}");
        }
        assert!(names.iter().all(|n| !n.starts_with("CARGO")), "{names:?}");
    }

    #[test]
    fn the_cargo_profile_environment_adds_the_cargo_variables() {
        let exec = SandboxedExec::new(config(), PathBuf::from("/w"), ExecProfile::Cargo);
        let env = exec.environment(Path::new("/tmp/scratch"), &[]);
        let names: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        for needed in [
            "CARGO_HOME",
            "CARGO_NET_OFFLINE",
            "CARGO_TARGET_DIR",
            "CARGO_INCREMENTAL",
        ] {
            assert!(names.contains(&needed), "{needed}");
        }
    }

    #[test]
    fn the_network_flag_needs_both_the_verb_and_the_caller() {
        let mut req = request(&["x"]);
        let denied = SandboxedExec::new(config(), PathBuf::from("/w"), ExecProfile::Generic);
        let allowed = denied.clone().with_network_allowed(true);
        let flag = |exec: &SandboxedExec, req: &ExecRequest| {
            parse_run(&exec.command_line(20001, req)[1..])
                .unwrap()
                .network
        };
        assert!(!flag(&denied, &req));
        assert!(!flag(&allowed, &req));
        req.network = true;
        assert!(!flag(&denied, &req));
        assert!(flag(&allowed, &req));
    }

    #[test]
    fn a_generic_run_executes_the_argv_without_a_shell_and_captures_output() {
        let dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let exec = SandboxedExec::new(
            fake_config(fake_helper(dir.path())),
            work.path().to_path_buf(),
            ExecProfile::Generic,
        );
        let outcome = exec
            .run(&request(&[
                "/bin/sh",
                "-c",
                "echo out; echo err >&2; pwd; exit 3",
            ]))
            .unwrap();
        assert!(!outcome.success);
        assert!(!outcome.timed_out);
        assert!(outcome.stdout.starts_with("out\n"));
        assert!(outcome
            .stdout
            .trim_end()
            .ends_with(work.path().file_name().unwrap().to_str().unwrap()));
        assert_eq!(outcome.stderr, "err\n");
        let literal = exec.run(&request(&["/bin/echo", "$HOME;", "`x`"])).unwrap();
        assert_eq!(literal.stdout, "$HOME; `x`\n");
        assert!(literal.success);
    }

    #[test]
    fn a_generic_run_sees_only_the_scrubbed_environment_and_never_touches_the_lockfile() {
        let dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let exec = SandboxedExec::new(
            fake_config(fake_helper(dir.path())),
            work.path().to_path_buf(),
            ExecProfile::Generic,
        );
        let mut req = request(&["/usr/bin/env"]);
        req.env.push(("STACK_VAR".to_string(), "1".to_string()));
        let outcome = exec.run(&req).unwrap();
        assert!(outcome.stdout.contains("STACK_VAR=1"));
        assert!(!outcome.stdout.contains("CARGO"));
        assert!(!work.path().join("Cargo.lock").exists());
    }

    #[test]
    fn a_cargo_run_freezes_with_a_lockfile_and_removes_a_placeholder_one() {
        let dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let cargo = SandboxedCargo::new(
            fake_config(fake_helper(dir.path())),
            work.path().to_path_buf(),
        );
        let outcome = cargo
            .run(&strings(&["/bin/echo", "cargo", "--offline"]))
            .unwrap();
        assert_eq!(outcome.stdout, "cargo --offline\n");
        assert!(!work.path().join("Cargo.lock").exists());
        std::fs::write(work.path().join("Cargo.lock"), "x").unwrap();
        let outcome = cargo
            .run(&strings(&["/bin/echo", "cargo", "--offline"]))
            .unwrap();
        assert_eq!(outcome.stdout, "cargo --frozen\n");
        let env = cargo.run(&strings(&["/usr/bin/env"])).unwrap().stdout;
        assert!(env.contains("CARGO_NET_OFFLINE=true"));
    }

    #[test]
    fn the_helper_is_asked_for_network_only_when_both_sides_allow_it() {
        let dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let exec = SandboxedExec::new(
            fake_config(fake_helper(dir.path())),
            work.path().to_path_buf(),
            ExecProfile::Generic,
        )
        .with_network_allowed(true);
        let mut req = request(&["/bin/true"]);
        exec.run(&req).unwrap();
        assert!(!fake_log(dir.path())
            .iter()
            .any(|l| l.starts_with("network")));
        req.network = true;
        exec.run(&req).unwrap();
        assert!(fake_log(dir.path()).contains(&"network true".to_string()));
    }

    #[test]
    fn a_timeout_kills_the_run_and_the_uid() {
        let dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let exec = SandboxedExec::new(
            fake_config(fake_helper(dir.path())),
            work.path().to_path_buf(),
            ExecProfile::Generic,
        );
        let mut req = request(&["/bin/sleep", "30"]);
        req.timeout = Some(Duration::from_millis(300));
        let started = Instant::now();
        let outcome = exec.run(&req).unwrap();
        assert!(outcome.timed_out);
        assert!(!outcome.success);
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(fake_log(dir.path()).iter().any(|l| l.starts_with("kill ")));
    }

    #[test]
    fn a_leased_run_keeps_the_scratch_and_reuses_the_uid() {
        let dir = tempfile::tempdir().unwrap();
        let work = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let exec = SandboxedExec::new(
            fake_config(fake_helper(dir.path())),
            work.path().to_path_buf(),
            ExecProfile::Generic,
        );
        exec.run_leased(
            20007,
            scratch.path(),
            &request(&["/bin/sh", "-c", "echo kept > $TMPDIR/f"]),
        )
        .unwrap();
        let second = exec
            .run_leased(
                20007,
                scratch.path(),
                &request(&["/bin/sh", "-c", "cat $TMPDIR/f"]),
            )
            .unwrap();
        assert_eq!(second.stdout, "kept\n");
        let log = fake_log(dir.path());
        assert_eq!(log.iter().filter(|l| *l == "run 20007").count(), 2);
        assert_eq!(log.iter().filter(|l| *l == "kill 20007").count(), 2);
    }
}
