static PATH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn hold_path_async() -> tokio::sync::MutexGuard<'static, ()> {
    PATH_LOCK.lock().await
}

#[cfg(all(test, unix))]
pub(crate) fn hold_path_blocking() -> tokio::sync::MutexGuard<'static, ()> {
    PATH_LOCK.blocking_lock()
}

#[cfg(unix)]
mod fake {
    use super::PATH_LOCK;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    fn settle_executable(bin: &Path) {
        for _ in 0..200 {
            match std::process::Command::new(bin)
                .arg("--settle")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
            {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                _ => return,
            }
        }
    }

    pub(crate) struct FakePodman {
        dir: TempDir,
        old_path: Option<OsString>,
        guard: Option<tokio::sync::MutexGuard<'static, ()>>,
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
                .map(|prefix| {
                    format!("case \"$*\" in \"{prefix}\"*) echo boom >&2; exit 1;; esac\n")
                })
                .unwrap_or_default();
            let script = format!(
                "#!/bin/sh\n\
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
            settle_executable(&bin);
            let _ = std::fs::remove_file(&log);
            let _ = std::fs::remove_file(&env_log);

            let old_path = std::env::var_os("PATH");
            let mut paths = vec![dir.path().to_path_buf()];
            if let Some(old) = &old_path {
                paths.extend(std::env::split_paths(old));
            }
            std::env::set_var("PATH", std::env::join_paths(paths).unwrap());
            Self {
                dir,
                old_path,
                guard: Some(guard),
            }
        }

        pub(crate) fn calls(&self) -> Vec<String> {
            read_lines(&self.dir.path().join("calls.log"))
        }

        pub(crate) fn env_file_contents(&self) -> Vec<String> {
            read_lines(&self.dir.path().join("env.log"))
        }
    }

    fn read_lines(path: &Path) -> Vec<String> {
        std::fs::read_to_string(PathBuf::from(path))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    impl FakePodman {
        #[cfg(test)]
        fn finish(mut self) -> tokio::sync::MutexGuard<'static, ()> {
            self.guard.take().expect("guard is held until finish")
        }
    }

    #[derive(Debug, PartialEq, Eq)]
    enum PathRestore {
        Set(OsString),
        Remove,
    }

    fn restore_action(old: Option<OsString>) -> PathRestore {
        match old {
            Some(old) => PathRestore::Set(old),
            None => PathRestore::Remove,
        }
    }

    fn apply_restore(name: &std::ffi::OsStr, action: PathRestore) {
        match action {
            PathRestore::Set(value) => std::env::set_var(name, value),
            PathRestore::Remove => std::env::remove_var(name),
        }
    }

    impl Drop for FakePodman {
        fn drop(&mut self) {
            apply_restore("PATH".as_ref(), restore_action(self.old_path.clone()));
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn restore_action_removes_an_originally_absent_path() {
            assert_eq!(restore_action(None), PathRestore::Remove);
        }

        fn unique_var() -> OsString {
            static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            OsString::from(format!("NANNA_TEST_RESTORE_{}_{n}", std::process::id()))
        }

        #[test]
        fn apply_restore_set_assigns_the_value() {
            let name = unique_var();
            apply_restore(&name, PathRestore::Set(OsString::from("v1")));
            let got = std::env::var_os(&name);
            std::env::remove_var(&name);
            assert_eq!(got, Some(OsString::from("v1")));
        }

        #[test]
        fn apply_restore_remove_unsets_the_variable() {
            let name = unique_var();
            std::env::set_var(&name, "present");
            apply_restore(&name, PathRestore::Remove);
            assert_eq!(std::env::var_os(&name), None);
        }

        #[test]
        fn restore_action_sets_an_originally_present_path() {
            assert_eq!(
                restore_action(Some(OsString::from("/usr/bin"))),
                PathRestore::Set(OsString::from("/usr/bin"))
            );
        }

        #[test]
        fn dropping_restores_the_original_path() {
            let guard = PATH_LOCK.blocking_lock();
            let original = std::env::var_os("PATH");
            let fake = FakePodman::with_guard(guard, None);
            assert_ne!(std::env::var_os("PATH"), original);
            let guard = fake.finish();
            let restored = std::env::var_os("PATH");
            drop(guard);
            assert_eq!(restored, original);
        }
    }
}

#[cfg(unix)]
pub(crate) use fake::FakePodman;
