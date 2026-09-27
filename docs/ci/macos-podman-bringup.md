# macOS podman-machine bring-up: why CI can't do it, and what can

## The short version

GitHub-hosted `arm64` macOS runners (`macos-15`, `macos-26`, and the
`macos-latest` alias, which now points to `macos-26` arm64 — `macos-14`
is deprecated) cannot run a podman machine, full stop. This is
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
2. **A real, still-open gap for the `libkrun` opt-in path.** Upstream
   podman 6 changed the *default* macOS machine provider to `libkrun`,
   but **Homebrew's own `podman` formula patches that back out** — its
   build log (from the first real `macos-intel-probe` run) shows it
   downloading `Patches/podman/revert-libkrun-default.patch` before
   building. Confirmed independently: the first real `macos-smoke` run's
   `podman machine list --format '{{.Name}} {{.VMType}}'` reported
   `podman-machine-default* applehv` — a bare `podman machine init` on
   Homebrew's podman 6.1.1 still creates an `applehv` machine, not
   `libkrun`, even with `krunkit` already on `PATH`. So this is *not*
   what breaks a default install. What it *does* break: Homebrew's
   `podman` formula still doesn't depend on `krunkit`, so anyone who
   explicitly runs `podman machine init --provider libkrun` (or inherits
   an old machine already configured that way) hits
   `krunkit: executable file not found in $PATH`
   ([Homebrew/homebrew-core#291552](https://github.com/homebrew/homebrew-core/issues/291552),
   open, unresolved as of this writing — and reproduced by a real
   `macos-intel-probe` build log, not just the issue report).
   `scripts/install.sh` now has a standalone `ensure_krunkit_macos()`
   step that installs `krunkit` on `arm64` whenever it's missing, so
   that opt-in path works. It doesn't change what a default install
   does (see finding 3's actual failure below).
3. **A third, previously-unreachable bug**, found by the first real
   `macos-smoke` run against this fix: `scripts/install.sh`'s
   `port_in_use_by()` uses `lsof -nP -iTCP:"$port" -sTCP:LISTEN`, which
   exits `1` (not just empty output) when nothing is listening on that
   port. Under `set -euo pipefail`, `AUDIT_PORT_8080_HOLDER="$(port_in_use_by
   8080)"` then kills the whole script instantly and silently on any
   clean macOS host — including this CI runner, which is why the first
   real run produced a completely empty log before `install_status: 1`.
   Linux never hit this because `port_in_use_by` prefers `ss`, whose
   `awk` filter exits `0` even with no match; macOS has no `ss`, so it
   always took the `lsof` branch. Fixed by appending `|| true`.

Fixing (1)-(3) does **not** make `macos-smoke` a full bring-up test
again. It makes the job fail for the right reason.

## What the fixed job actually shows (real `macos-15` run)

With (1)-(3) fixed, `macos-smoke` passes, and its log is the real
confirmation this document is built on: `podman` (6.1.1) and `krunkit`
(1.3.2, plus its 7 dependencies) both installed cleanly from Homebrew
bottles in seconds, `podman machine init` succeeded, and then:

```
==> starting podman machine...
Starting machine "podman-machine-default"
Error: vfkit exited unexpectedly with exit code 1
```

This is *exactly* the failure this job's predecessor's own comments
described before this job started dying on the formula typo first
("applehv/vfkit aborts on start ... verified rounds 7/8") — now
independently reconfirmed with real evidence instead of institutional
memory. `podman machine list --format '{{.Name}} {{.VMType}}'` reported
`podman-machine-default* applehv`, matching finding (2) above.

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

### Intel-hosted runners: virtualization works, Homebrew's podman doesn't support Intel macOS at all any more

GitHub's docs hedge the nested-virtualization limitation as an `arm64`
constraint. **Confirmed empirically** by the first real
`macos-intel-probe` run on `macos-15-intel`: `sysctl kern.hv_support`
returns `1` — Hypervisor.framework genuinely *is* available on this
runner (`kern.hv_support` reported "unknown oid" on the arm64 runner in
the same workflow run — see the caveat in `macos-smoke`'s own log about
not over-generalizing that to all Apple Silicon). This is a real,
citable difference between the two architectures on GitHub's hosted
fleet, not a wording ambiguity.

But Homebrew's `podman` formula is unusable on Intel macOS regardless.
First attempt (plain `brew install podman`) failed with
`podman: no bottle available!`, flagged "Tier 3". Second attempt
(`brew install --build-from-source podman`, since Tier 3 formulae are
still formally buildable) failed harder and more definitively:

```
podman: The arm64 architecture is required for this software.
##[error]podman: An unsatisfied requirement failed this build.
```

Homebrew's `podman` formula now hard-requires `arch: :arm64` — it isn't
a missing-bottle inconvenience, it's a formula-level block. **This means
no real Intel Mac user can install podman via Homebrew's official
formula today, independent of CI or GitHub Actions entirely** — this is
a maintainer-facing finding, not just a CI one. Getting a working Intel
lane (in CI or for a real user) would need a source outside Homebrew's
main formula — e.g. podman's own official `.pkg`/tarball releases from
[github.com/containers/podman/releases](https://github.com/containers/podman/releases) —
which is untried and out of scope for this PR; see "What's still open"
in the PR description.

`macos-intel-probe` is intentionally **not** wired into
`install-test-gate`. Its real value going forward is the `kern.hv_support`
diagnostic, not (yet) a working bring-up.

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

**Setup**, if the maintainer chooses to do this: register the runner on
a separate private repo, not `nanna-coder` itself — see the security
section below for why that's the one option that actually closes the
exposure, not just reduces it.

1. On that private repo: Settings → Actions → Runners → "New
   self-hosted runner", choose macOS, follow the generated `config.sh`
   steps on the Mac. Label it (e.g. `nanna-macos-arm64`) in addition to
   the default `self-hosted`/`macOS`/`ARM64` labels GitHub assigns
   automatically.
2. That private repo's workflow checks out `nanna-coder`'s `main` (via
   `actions/checkout` with `repository: DominicBurkart/nanna-coder`,
   `ref: main`) on a schedule or `workflow_dispatch`, then targets the
   runner with:
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

Restricting *this specific job's* `on:` trigger to `push`/`workflow_dispatch`
only is **necessary but not sufficient**, and it's important to be
precise about why: a self-hosted runner registered on this repo will
pick up *any* queued job whose labels match, from *any* workflow file
in the repo — including one a fork PR adds itself. A contributor can
open a PR that adds a brand-new `.github/workflows/evil.yml` with its
own `on: pull_request` trigger and `runs-on: [self-hosted, macOS, ...]`.
Restricting the *existing* macOS job's trigger does nothing to stop
that new file from targeting the same runner. There is no workflow-YAML
setting that closes this by itself — it's why GitHub's guidance is "use
GitHub-hosted runners for public repos," not "restrict your triggers."
The option that actually removes the exposure, plus two that reduce it:

- **Register the runner on a separate private repo, not `nanna-coder`
  itself.** A self-hosted runner registered at the repository level is
  scoped to that one repository — it cannot pick up jobs queued by any
  other repo, including `nanna-coder`
  ([docs.github.com/en/actions/reference/runners/self-hosted-runners](https://docs.github.com/en/actions/reference/runners/self-hosted-runners)).
  So: register the runner on a small private repo the maintainer
  controls, whose only workflow checks out `nanna-coder`'s `main` (via
  `actions/checkout` with `repository: DominicBurkart/nanna-coder`,
  `ref: main`) on a schedule or `workflow_dispatch`. No `nanna-coder`
  fork PR can ever queue a job against that runner, because the runner
  was never registered on `nanna-coder` in the first place — this is
  the one option here that isn't "reduce the odds," it structurally
  can't be reached by a fork PR at all.
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
  Only relevant at all if the runner is registered directly on
  `nanna-coder`, which the option above avoids doing.
- **Treat the runner host as untrusted-adjacent regardless**: a
  dedicated macOS user account with no stored credentials/secrets
  beyond what the specific job needs, and prefer ephemeral/JIT runner
  registration (re-registers per job, wiped after) over a persistent
  always-on runner, so a compromise doesn't persist across runs. This
  is a persistence control, not a targeting control — it doesn't stop a
  job from being queued against the runner, only limits what survives
  if one runs.

Runner groups with repository/workflow restrictions are a GitHub
*organization*-level feature; `DominicBurkart/nanna-coder` is a
personal-account repository, so that control isn't available here
unless the repo is transferred into an org — the private-repo option
above is the personal-account equivalent.

None of this is set up by this PR. It's documented here as a
well-specified option for the maintainer to decide on, not attempted.

## If GitHub ever changes this

`macos-smoke`'s classification step (see the job's own comments in
`install-test.yml`) emits an `::warning::` and exits 0 if podman machine
bring-up ever unexpectedly succeeds — that's the signal to revisit this
document and restore a real `macos-full-bringup` job. Likewise for
`macos-intel-probe` on the Intel side.
