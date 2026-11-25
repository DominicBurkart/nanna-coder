use crate::exec_sandbox::{next_uid, ExecRequest, Outcome, SandboxedExec};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static LIVE_UIDS: Mutex<Option<HashSet<u32>>> = Mutex::new(None);

fn claim_uid() -> u32 {
    let mut guard = LIVE_UIDS.lock().unwrap_or_else(|e| e.into_inner());
    let live = guard.get_or_insert_with(HashSet::new);
    loop {
        let uid = next_uid();
        if live.insert(uid) {
            return uid;
        }
    }
}

fn release_uid(uid: u32) {
    let mut guard = LIVE_UIDS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(live) = guard.as_mut() {
        live.remove(&uid);
    }
}

pub struct SandboxLease {
    exec: SandboxedExec,
    uid: u32,
    root: PathBuf,
    gate: Mutex<()>,
}

impl SandboxLease {
    pub fn allocate(exec: SandboxedExec) -> std::io::Result<Self> {
        Self::allocate_in(exec, &std::env::temp_dir())
    }

    pub fn allocate_in(exec: SandboxedExec, parent: &Path) -> std::io::Result<Self> {
        let uid = claim_uid();
        let root = parent.join(format!("nanna-lease-{uid}-{}", std::process::id()));
        for dir in ["target", "home", "tmp"] {
            if let Err(error) = std::fs::create_dir_all(root.join(dir)) {
                let _ = std::fs::remove_dir_all(&root);
                release_uid(uid);
                return Err(error);
            }
        }
        Ok(Self {
            exec,
            uid,
            root,
            gate: Mutex::new(()),
        })
    }

    pub fn uid(&self) -> u32 {
        self.uid
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn exec(&self) -> &SandboxedExec {
        &self.exec
    }

    pub fn run(&self, request: &ExecRequest) -> std::io::Result<Outcome> {
        let _turn = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        self.exec.run_leased(self.uid, &self.root, request)
    }
}

impl Drop for SandboxLease {
    fn drop(&mut self) {
        self.exec.kill_uid(self.uid);
        let _ = std::fs::remove_dir_all(&self.root);
        release_uid(self.uid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec_sandbox::tests::{fake_config, fake_helper, fake_log};
    use crate::exec_sandbox::{ExecProfile, ExecRequest, SandboxedExec, UID_BASE, UID_SPAN};

    struct Rig {
        helper_dir: tempfile::TempDir,
        scratch_parent: tempfile::TempDir,
        work: tempfile::TempDir,
    }

    fn rig() -> Rig {
        Rig {
            helper_dir: tempfile::tempdir().unwrap(),
            scratch_parent: tempfile::tempdir().unwrap(),
            work: tempfile::tempdir().unwrap(),
        }
    }

    fn exec(rig: &Rig) -> SandboxedExec {
        SandboxedExec::new(
            fake_config(fake_helper(rig.helper_dir.path())),
            rig.work.path().to_path_buf(),
            ExecProfile::Generic,
        )
    }

    fn lease(rig: &Rig) -> SandboxLease {
        SandboxLease::allocate_in(exec(rig), rig.scratch_parent.path()).unwrap()
    }

    fn request(parts: &[&str]) -> ExecRequest {
        ExecRequest::new(parts.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn a_lease_owns_a_scratch_root_with_target_home_and_tmp() {
        let rig = rig();
        let lease = lease(&rig);
        assert!(lease.root().starts_with(rig.scratch_parent.path()));
        for dir in ["target", "home", "tmp"] {
            assert!(lease.root().join(dir).is_dir(), "{dir}");
        }
        assert!((UID_BASE..UID_BASE + UID_SPAN).contains(&lease.uid()));
    }

    #[test]
    fn two_live_leases_never_share_a_uid_or_root() {
        let rig = rig();
        let leases: Vec<SandboxLease> = (0..50).map(|_| lease(&rig)).collect();
        let uids: std::collections::HashSet<u32> = leases.iter().map(|l| l.uid()).collect();
        let roots: std::collections::HashSet<&Path> = leases.iter().map(|l| l.root()).collect();
        assert_eq!(uids.len(), leases.len());
        assert_eq!(roots.len(), leases.len());
    }

    #[test]
    fn every_call_reuses_the_uid_and_keeps_the_scratch() {
        let rig = rig();
        let lease = lease(&rig);
        lease
            .run(&request(&["/bin/sh", "-c", "echo kept > $TMPDIR/f"]))
            .unwrap();
        let second = lease
            .run(&request(&["/bin/sh", "-c", "cat $TMPDIR/f"]))
            .unwrap();
        assert_eq!(second.stdout, "kept\n");
        let uid = lease.uid();
        let log = fake_log(rig.helper_dir.path());
        assert_eq!(
            log.iter().filter(|l| **l == format!("run {uid}")).count(),
            2
        );
    }

    #[test]
    fn the_uid_is_killed_after_each_call() {
        let rig = rig();
        let lease = lease(&rig);
        let kill = format!("kill {}", lease.uid());
        lease.run(&request(&["/bin/true"])).unwrap();
        assert_eq!(
            fake_log(rig.helper_dir.path())
                .iter()
                .filter(|l| **l == kill)
                .count(),
            1
        );
        lease.run(&request(&["/bin/true"])).unwrap();
        assert_eq!(
            fake_log(rig.helper_dir.path())
                .iter()
                .filter(|l| **l == kill)
                .count(),
            2
        );
    }

    #[test]
    fn dropping_a_lease_removes_the_root_and_kills_the_uid() {
        let rig = rig();
        let lease = lease(&rig);
        let root = lease.root().to_path_buf();
        let kill = format!("kill {}", lease.uid());
        lease
            .run(&request(&["/bin/sh", "-c", "echo x > $TMPDIR/f"]))
            .unwrap();
        let before = fake_log(rig.helper_dir.path())
            .iter()
            .filter(|l| **l == kill)
            .count();
        drop(lease);
        assert!(!root.exists());
        let after = fake_log(rig.helper_dir.path())
            .iter()
            .filter(|l| **l == kill)
            .count();
        assert_eq!(after, before + 1);
    }

    #[test]
    fn concurrent_calls_on_one_lease_are_serialised() {
        let rig = rig();
        let lease = std::sync::Arc::new(lease(&rig));
        let marker = rig.work.path().join("running");
        let script = format!(
            "if [ -e {m} ]; then echo overlap; fi; touch {m}; sleep 0.3; rm {m}",
            m = marker.display()
        );
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let lease = std::sync::Arc::clone(&lease);
                let script = script.clone();
                std::thread::spawn(move || {
                    lease
                        .run(&request(&["/bin/sh", "-c", script.as_str()]))
                        .unwrap()
                })
            })
            .collect();
        for handle in handles {
            assert_eq!(handle.join().unwrap().stdout, "");
        }
    }

    #[test]
    fn a_timeout_through_the_lease_reports_timed_out() {
        let rig = rig();
        let lease = lease(&rig);
        let mut req = request(&["/bin/sleep", "30"]);
        req.timeout = Some(std::time::Duration::from_millis(300));
        let outcome = lease.run(&req).unwrap();
        assert!(outcome.timed_out);
    }
}
