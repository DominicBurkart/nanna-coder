# macOS podman-machine bring-up: why CI can't do it, and what can

## The short version

GitHub-hosted `arm64` macOS runners (`macos-14`, `macos-15`, and their
`macos-latest` alias) cannot run a podman machine, full stop. This is
not a packaging problem — no combination of podman machine provider
(`applehv`/vfkit, `qemu`, `libkrun`/krunkit) will ever work there,
because those runners don't expose Hypervisor.framework / nested
virtualization to the guest at all. `.github/workflows/install-test.yml`
`macos-smoke` reflects that: it verifies `scripts/install.sh` detects
macOS correctly and drives podman machine bring-up far enough to hit
the real, known failure, instead of quietly dying on an unrelated
packaging bug (which is what was happening before this was fixed —
`brew install podman libkrun` failed because `libkrun` was never a real
Homebrew formula name).

## What was actually broken

The job's failure was masking two separate problems:

1. **A typo'd formula name.** `brew install podman libkrun` failed in
   ~30 seconds because no Homebrew formula named `libkrun` has ever
   existed. The correct package for a libkrun-backed podman machine
   provider is `krunkit`, from a dedicated tap:
   ```
   brew tap libkrun/krun
   brew install krunkit
   ```
   (verified against the tap's own `Formula/krunkit.rb` at
   [libkrun/homebrew-krun](https://github.com/libkrun/homebrew-krun) —
   note the tap moved from the older `slp/krun` name; even
   [krunkit's own README](https://github.com/containers/krunkit/blob/main/README.md)
   still documents the stale `slp/krun` tap as of this writing, so trust
   the tap repo's own README over krunkit's). Homebrew 6.0 (June 2026)
   also requires trusting
   third-party taps before installing from them
   ([docs.brew.sh/Tap-Trust](https://docs.brew.sh/Tap-Trust)):
   `brew trust libkrun/krun`.
2. **A real gap in `scripts/install.sh`.** Podman 6 (current stable,
   confirmed via `formulae.brew.sh/api/formula/podman.json`) changed
   the default macOS machine provider from `applehv` to `libkrun`, but
   Homebrew's `podman` formula does not depend on `krunkit`. So
   `brew install podman` followed by `podman machine init` now fails
   for *any* macOS user on Apple Silicon with
   `krunkit: executable file not found in $PATH`
   ([Homebrew/homebrew-core#291552](https://github.com/homebrew/homebrew-core/issues/291552),
   open, unresolved as of the writing of this doc). `scripts/install.sh`
   now has a standalone `ensure_krunkit_macos()` step that installs
   `krunkit` on `arm64` whenever it's missing — not only when podman
   itself was just installed — so a real Apple Silicon user (who *does*
   have working Hypervisor.framework access, unlike a GitHub-hosted
   runner) gets a working podman machine instead of this error.

Fixing (1) and (2) does **not** make `macos-smoke` a full bring-up test
again. It makes the job fail for the right reason.

## Why no provider works on GitHub-hosted arm64 runners

From GitHub's own docs
([docs.github.com/en/actions/reference/runners/github-hosted-runners](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)):

> Nested-virtualization is not supported due to the limitation of
> Apple's Virtualization Framework.

This is stated to apply specifically to `arm64` macOS runners.
[actions/runner-images#13505](https://github.com/actions/runner-images/issues/13505),
opened January 2026 and asking GitHub to expose Hypervisor.framework
passthrough on Apple Silicon runners, was **closed as "not planned."**
An earlier request from March 2024
([actions/runner-images#9460](https://github.com/actions/runner-images/issues/9460))
was closed for the same reason. This is a current, dated, first-party
statement — not outdated forum chatter — and it is the load-bearing
fact behind this whole document: every podman machine provider
(`applehv`/vfkit, `qemu`, `libkrun`/krunkit) needs exactly the
Hypervisor.framework access these runners don't expose, so trying
"yet another provider" cannot fix this.

`krunkit` is additionally `arm64`-only by its own Homebrew formula
(`depends_on arch: :arm64`, "libkrun ... only supports
Hypervisor.framework on arm64"), so it was never going to be usable on
an Intel runner regardless of the above.

### Open question: Intel-hosted runners

GitHub's docs hedge the nested-virtualization limitation as an `arm64`
constraint, and GitHub-hosted macOS Intel runners
(`macos-15-intel`/`macos-26-intel`) are free for this public repo. It's
not yet established whether Intel-hosted runners expose real
Hypervisor.framework access (podman's `applehv` provider is not
`libkrun`/Apple-Silicon-specific — `qemu` was removed in podman 5.x, so
`applehv` is the only remaining option to test on Intel). The
`macos-intel-probe` job in `install-test.yml` checks this empirically
(`sysctl kern.hv_support`, then a real `podman machine init && start`
with `CONTAINERS_MACHINE_PROVIDER=applehv`). It is intentionally **not**
wired into `install-test-gate`: as of this writing no run of it has
been observed to succeed, so its result carries no signal either way.
If a run of `macos-intel-probe` does succeed, that changes this
document's conclusion for the Intel case specifically, and the job
should be promoted into a real full bring-up lane (mirroring
`linux-bringup`) and folded into the gate.

## What would actually give a real signal

### Option A: the maintainer's own Apple Silicon hardware

Run the real install path by hand on real hardware:

```
brew tap libkrun/krun
brew trust libkrun/krun
brew install podman krunkit
podman machine init --cpus=2 --memory=3072 --disk-size=10
podman machine start
bash scripts/install.sh --no-pull --skip-model-pull --yes \
  --harness-image <ref> --ollama-image <ref>
```

This is the fastest, lowest-risk way to confirm the real macOS install
path works, since it needs no GitHub configuration and carries none of
the security exposure of Option B. It's a one-time manual check, not
something that needs to become recurring CI — the actual podman machine
provider logic in `scripts/install.sh` doesn't otherwise differ by OS
version in a way that would silently regress between checks.

### Option B: a self-hosted runner on real Apple Silicon hardware

This would let a real `macos-full-bringup` job run in CI, on every PR,
on real hardware. It requires the repo owner's own Mac and GitHub admin
access on the repo — this cannot be set up by an agent working in this
sandbox.

**Setup**, if the maintainer chooses to do this:

1. Repo → Settings → Actions → Runners → "New self-hosted runner",
   choose macOS, follow the generated `config.sh` steps on the Mac.
   Label it (e.g. `nanna-macos-arm64`) in addition to the default
   `self-hosted`/`macOS`/`ARM64` labels GitHub assigns automatically.
2. Target it from a workflow with:
   ```yaml
   runs-on: [self-hosted, macOS, ARM64, nanna-macos-arm64]
   ```

**The security problem, and why it changes the design, not just the
label.** GitHub's own guidance
([docs.github.com/en/actions/reference/security/secure-use](https://docs.github.com/en/actions/reference/security/secure-use))
is blunt:

> Self-hosted runners should almost never be used for public
> repositories... anyone who can fork the repository and open a pull
> request ... are able to compromise the self-hosted runner
> environment, including gaining access to secrets and the
> `GITHUB_TOKEN`.

`nanna-coder` is public, so this applies directly, and one mitigation
that sounds plausible **must not be used**:

- **`pull_request_target` with a required-reviewer-approval gate**,
  which the original scoping of this task suggested as a mitigation.
  This is the wrong tool here, for a specific, citable reason: GitHub's
  own docs on fork-workflow approval state that "workflows triggered by
  `pull_request_target` events are run in the context of the base
  branch... [they] will always run, regardless of approval settings"
  ([github/docs: approve-runs-from-forks](https://github.com/github/docs/blob/main/content/actions/how-tos/manage-workflow-runs/approve-runs-from-forks.md)).
  A `pull_request_target` job also runs with the base repo's
  `GITHUB_TOKEN` and secrets while still being able to check out
  attacker-controlled PR head code if the workflow does so — the
  well-known "pwn request" pattern. It should never gate a self-hosted
  job on a public repo, approval setting or not.

Restricting the job's own `on:` trigger to `push`/`workflow_dispatch`
only (no `pull_request`, no `pull_request_target`) is the actual,
necessary control: it's not that the trigger keyword is magic, it's
that a self-hosted runner must never evaluate code from an
unreviewed/unmerged fork PR at all, and only `push`-on-protected-branch
and manual `workflow_dispatch` guarantee that. Beyond that:

- **The repo-level "Approval for running fork pull request workflows
  from contributors" setting** (Settings → Actions → General), set to
  "Require approval for all external contributors", adds a required
  human-approval step before *any* workflow runs code from a fork PR —
  but GitHub's own docs explicitly warn this is **not sufficient by
  itself for self-hosted runners**: "If you are using self-hosted
  runners, potentially malicious user-controlled workflow code will
  execute automatically if the user is allowed to bypass approval...
  or if the pull request is approved. You must consider the risk...
  and should review and follow the self-hosted runner security
  recommendations regardless of the approval settings utilized."
  ([same source](https://github.com/github/docs/blob/main/content/actions/how-tos/manage-workflow-runs/approve-runs-from-forks.md)).
  So this setting is a useful additional layer (a human must look at a
  PR before its workflow runs at all) — it is not, on its own, a reason
  to combine `pull_request` triggers with a self-hosted runner.
- **Runner groups with repository/workflow restrictions** are a GitHub
  *organization*-level feature. `DominicBurkart/nanna-coder` is a
  personal-account repository, so this control is not available here
  unless the repo is transferred into an org.
- **Treat the runner host as untrusted-adjacent regardless**: a
  dedicated macOS user account with no stored credentials/secrets
  beyond what the specific job needs, and prefer ephemeral/JIT runner
  registration (re-registers per job, wiped after) over a persistent
  always-on runner, so a compromise doesn't persist across runs.

None of this is set up by this PR. It's documented here as a
well-specified option for the maintainer to decide on, not attempted.

## If GitHub ever changes this

`macos-smoke`'s classification step (see the job's own comments in
`install-test.yml`) emits an `::warning::` and exits 0 if podman machine
bring-up ever unexpectedly succeeds — that's the signal to revisit this
document and restore a real `macos-full-bringup` job. Likewise for
`macos-intel-probe` on the Intel side.
