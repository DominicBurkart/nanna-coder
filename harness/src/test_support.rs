use std::ffi::OsString;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

static PATH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) struct FakePodman {
    dir: TempDir,
    old_path: Option<Option<OsString>>,
    _guard: tokio::sync::MutexGuard<'static, ()>,
}

impl FakePodman {
    pub(crate) fn install(fail_on: Option<&str>) -> Self {
        Self::with_guard(PATH_LOCK.blocking_lock(), fail_on)
    }

    pub(crate) async fn install_async(fail_on: Option<&str>) -> Self {
        Self::with_guard(PATH_LOCK.lock().await, fail_on)
    }

    fn with_guard(guard: tokio::sync::MutexGuard<'static, ()>, fail_on: Option<&str>) -> Self {
        let dir = TempDir::new().unwrap();
        let log = dir.path().join("calls.log");
        let env_log = dir.path().join("env.log");
        let fail_case = fail_on
            .map(|prefix| format!("case \"$*\" in \"{prefix}\"*) echo boom >&2; exit 1;; esac\n"))
            .unwrap_or_default();
        let script = format!(
            "#!/bin/sh\n\
             [ -n \"$NANNA_FAKE_PODMAN_PROBE\" ] && exit 0\n\
             echo \"$@\" >> '{log}'\n\
             prev=\n\
             for a in \"$@\"; do\n\
             if [ \"$prev\" = \"--env-file\" ]; then cat \"$a\" >> '{env_log}'; fi\n\
             prev=$a\n\
             done\n\
             {fail_case}\
             exit 0\n",
            log = log.display(),
            env_log = env_log.display(),
        );
        let bin = dir.path().join("podman");
        std::fs::write(&bin, script).unwrap();
        let mut perms = std::fs::metadata(&bin).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&bin, perms).unwrap();
        wait_until_executable(&bin);

        let old_path = std::env::var_os("PATH");
        let mut paths = vec![dir.path().to_path_buf()];
        if let Some(old) = &old_path {
            paths.extend(std::env::split_paths(old));
        }
        std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
        Self {
            dir,
            old_path: Some(old_path),
            _guard: guard,
        }
    }

    pub(crate) fn calls(&self) -> Vec<String> {
        read_lines(&self.dir.path().join("calls.log"))
    }

    pub(crate) fn env_file_contents(&self) -> Vec<String> {
        read_lines(&self.dir.path().join("env.log"))
    }
}

fn wait_until_executable(bin: &Path) {
    const ETXTBSY: i32 = 26;
    for _ in 0..2000 {
        match std::process::Command::new(bin)
            .env("NANNA_FAKE_PODMAN_PROBE", "1")
            .status()
        {
            Err(e) if e.raw_os_error() == Some(ETXTBSY) => {
                std::thread::sleep(std::time::Duration::from_millis(5))
            }
            Ok(_) => return,
            Err(e) => panic!("fake podman is not executable: {e}"),
        }
    }
    panic!("fake podman stayed busy");
}

fn read_lines(path: &Path) -> Vec<String> {
    std::fs::read_to_string(PathBuf::from(path))
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

#[derive(Debug, PartialEq, Eq)]
enum PathRestore {
    Set(OsString),
    Remove,
    Keep,
}

fn restore_action(old: Option<Option<OsString>>) -> PathRestore {
    match old {
        Some(Some(old)) => PathRestore::Set(old),
        Some(None) => PathRestore::Remove,
        None => PathRestore::Keep,
    }
}

impl FakePodman {
    fn restore_path(&mut self) {
        match restore_action(self.old_path.take()) {
            PathRestore::Set(old) => std::env::set_var("PATH", old),
            PathRestore::Remove => std::env::remove_var("PATH"),
            PathRestore::Keep => {}
        }
    }
}

impl Drop for FakePodman {
    fn drop(&mut self) {
        self.restore_path();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_action_removes_an_originally_absent_path() {
        assert_eq!(restore_action(Some(None)), PathRestore::Remove);
    }

    #[test]
    fn restore_action_sets_an_originally_present_path() {
        assert_eq!(
            restore_action(Some(Some(OsString::from("/usr/bin")))),
            PathRestore::Set(OsString::from("/usr/bin"))
        );
    }

    #[test]
    fn restore_action_keeps_when_already_restored() {
        assert_eq!(restore_action(None), PathRestore::Keep);
    }

    #[test]
    fn dropping_restores_the_original_path() {
        let guard = PATH_LOCK.blocking_lock();
        let original = std::env::var_os("PATH");
        let mut fake = FakePodman::with_guard(guard, None);
        assert_ne!(std::env::var_os("PATH"), original);
        fake.restore_path();
        assert_eq!(std::env::var_os("PATH"), original);
        drop(fake);
    }
}
