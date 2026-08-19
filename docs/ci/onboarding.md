# CI Onboarding (for new maintainers)

Welcome. This is the shortest path to being productive on Nanna Coder's
CI. Plan ~2 hours.

## Step 0: Prereqs

- Push access to `github.com/DominicBurkart/nanna-coder` (or a fork
  you're sending PRs from).
- Local Nix install (`DeterminateSystems/nix-installer`). The whole CI
  runs Nix; if you can't run `nix build` locally you can't debug
  failures.
- Podman or Docker for container-related work.

## Step 1: Read these in order

1. [`AGENTS.md`](../../AGENTS.md) — repo conventions and the codecov
   guard policy.
2. [`ARCHITECTURE.md`](../../ARCHITECTURE.md) — the system you're
   testing.
3. [`docs/ci-cd-pipeline.md`](../ci-cd-pipeline.md) — long-form pipeline
   tour.
4. [`docs/ci/architecture.md`](architecture.md) — workflow map.
5. [`docs/ci/troubleshooting.md`](troubleshooting.md) — skim; you'll
   come back when something breaks.

## Step 2: Reproduce CI locally

```bash
# Clone
git clone https://github.com/DominicBurkart/nanna-coder
cd nanna-coder

# Enter the dev shell
nix develop

# Run the same commands CI runs
cargo nextest run --workspace --lib --all-features            # unit
cargo nextest run --workspace --test '*' --all-features       # integration
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
nix build .#harnessImage                                      # container
```

If any of these fail locally, your environment is wrong, not CI. Fix
environment before debugging CI.

## Step 3: Walk one workflow end-to-end

Open `.github/workflows/ci.yml` and trace one PR's worth of jobs:
- `test-matrix` fans out into 7 matrix combos.
- Each succeeds → `build-matrix` (4 targets) and `build-containers`
  (2 images) fan out.
- `security-scan` and `cache-maintenance` run conditionally.
- `all-checks` aggregates everything; this is what branch protection
  requires.

Then read `jobs.all-checks` carefully. The "Verify gate covers every
job" step is the canary that prevents new jobs from sneaking past the
gate.

## Step 4: Understand the codecov contract

Read the `# Run security checks` step comment in `ci.yml`. It explains
*why* tarpaulin runs with `--workspace --all-features` (not
`--ignore-tests`), and why those choices matter for codecov's patch
metric. This is the most-easily-broken contract in the repo.

Then read `codecov.yml` and `.github/workflows/codecov-guard.yml`. The
guard prevents silent relaxations; you cannot weaken it without admin
bypass.

## Step 5: Make a no-op change

Open a PR that touches `README.md` only. Watch the checks. You should
see:
- `test-matrix` (×7) running
- `build-matrix` (×4) waiting on test-matrix
- `build-containers` (×2) waiting on test-matrix
- `All Checks Passed` aggregating

Average wall-clock on a warm cache: ~15 min. Cold cache: ~45 min.

## Step 6: Bookmark these

- Actions tab: https://github.com/DominicBurkart/nanna-coder/actions
- Cachix dashboard: https://app.cachix.org/cache/nanna-coder
- Codecov dashboard: https://app.codecov.io/gh/DominicBurkart/nanna-coder

## Step 7: On-call rituals

- **Red main**: drop everything. Find the breaking commit
  (`git bisect` if needed), open a revert PR. Don't try to fix-forward
  unless the fix is trivial and reviewed.
- **Slow PR builds**: check Cachix dashboard for cache hit rate. If
  it's <80%, run `cache-warming.yml` manually.
- **Flaky test**: open an issue, mark `#[ignore]` with a `// FLAKY: see
  #N` comment. Don't silently retry.

## Where to ask for help

- Architecture questions: re-read `ARCHITECTURE.md` first, then open a
  discussion.
- Workflow YAML questions: GitHub Actions docs. The `actions/checkout`,
  `actions/upload-artifact`, and `cachix/cachix-action` READMEs are the
  most useful.
- Nix questions: the determinate-systems forum or the NixOS Discourse.
