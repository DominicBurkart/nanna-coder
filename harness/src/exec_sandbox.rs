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
    pub program: Vec<String>,
}

pub fn parse_run(args: &[String]) -> Result<RunSpec, String> {
    let mut spec = RunSpec {
        uid: 0,
        cpu_secs: DEFAULT_CPU_SECS,
        fsize_bytes: DEFAULT_FSIZE_BYTES,
        hidden: Vec::new(),
        chdir: PathBuf::from("/"),
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
    let flags = libc::CLONE_NEWUSER
        | libc::CLONE_NEWNET
        | libc::CLONE_NEWIPC
        | libc::CLONE_NEWUTS
        | libc::CLONE_NEWNS;
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
    let usage = "usage: nanna-sandbox selftest | kill-uid UID | run [--uid U --hide DIR --chdir DIR --cpu-secs N --fsize-bytes N] -- PROGRAM ARGS";
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
        Some(Self {
            helper: env.helper.clone()?,
            cargo_home: env.cargo_home.clone()?,
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

#[derive(Debug, Clone)]
pub struct SandboxedCargo {
    config: SandboxConfig,
    workspace: PathBuf,
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

fn own_dir(path: &Path, uid: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    let c = CString::new(path.as_os_str().as_encoded_bytes())?;
    if unsafe { libc::chown(c.as_ptr(), uid, uid) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

impl SandboxedCargo {
    pub fn new(config: SandboxConfig, workspace: PathBuf) -> Self {
        Self { config, workspace }
    }

    pub fn command_line(&self, uid: u32, argv: &[String]) -> Vec<String> {
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
        for hidden in &self.config.hidden {
            line.push("--hide".to_string());
            line.push(hidden.display().to_string());
        }
        line.push("--".to_string());
        line.extend(argv.iter().cloned());
        line
    }

    pub fn environment(&self, target: &Path) -> Vec<(String, String)> {
        let home = target.join("home");
        vec![
            ("PATH".to_string(), self.config.path.clone()),
            (
                "CARGO_HOME".to_string(),
                self.config.cargo_home.display().to_string(),
            ),
            ("CARGO_NET_OFFLINE".to_string(), "true".to_string()),
            (
                "CARGO_TARGET_DIR".to_string(),
                target.join("target").display().to_string(),
            ),
            ("HOME".to_string(), home.display().to_string()),
            (
                "TMPDIR".to_string(),
                target.join("tmp").display().to_string(),
            ),
            ("CARGO_INCREMENTAL".to_string(), "0".to_string()),
            ("CARGO_TERM_COLOR".to_string(), "never".to_string()),
            ("LANG".to_string(), "C.UTF-8".to_string()),
        ]
    }

    pub fn run(&self, argv: &[String]) -> std::io::Result<Outcome> {
        let uid = next_uid();
        let scratch =
            std::env::temp_dir().join(format!("nanna-cargo-{uid}-{}", std::process::id()));
        let lock = self.workspace.join("Cargo.lock");
        let had_lock = lock.exists();
        let argv = lock_flags(argv, had_lock);
        if !had_lock {
            std::fs::write(&lock, b"")?;
            let c = CString::new(lock.as_os_str().as_encoded_bytes())?;
            if unsafe { libc::chown(c.as_ptr(), uid, uid) } != 0 {
                let _ = std::fs::remove_file(&lock);
                return Err(std::io::Error::last_os_error());
            }
        }
        let result = self.run_in(uid, &scratch, &argv);
        if !had_lock {
            let _ = std::fs::remove_file(&lock);
        }
        let _ = Command::new(&self.config.helper)
            .args(["kill-uid", &uid.to_string()])
            .env_clear()
            .status();
        let _ = std::fs::remove_dir_all(&scratch);
        result
    }

    fn run_in(&self, uid: u32, scratch: &Path, argv: &[String]) -> std::io::Result<Outcome> {
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
            .args(self.command_line(uid, argv))
            .env_clear()
            .envs(self.environment(scratch))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        let mut child = command.spawn()?;
        let out = drain(child.stdout.take().expect("piped stdout"));
        let err = drain(child.stderr.take().expect("piped stderr"));
        let deadline = Instant::now() + self.config.timeout;
        let mut timed_out = false;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                timed_out = true;
                unsafe { libc::killpg(child.id() as i32, libc::SIGKILL) };
                let _ = Command::new(&self.config.helper)
                    .args(["kill-uid", &uid.to_string()])
                    .env_clear()
                    .status();
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
mod tests {
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
}
