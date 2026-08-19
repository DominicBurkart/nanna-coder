# CI Maintenance

Routine work to keep the pipeline healthy. Schedule these so they don't
accumulate.

## Weekly

- **Review `ci-integration.yml` results from the Monday cron run.** This
  is the canary for upstream action and runner-image drift. A green
  weekly run means the cold-cache fallback still works and the negative
  test still detects failures. A red run is almost always a signal that
  something upstream changed (action version, runner image, Nix
  installer).
- **Scan workflow run durations.** Open the Actions tab, sort by
  duration, look for outliers. The `ci-metrics.yml` summary aggregates
  this — read its step-summary on the most recent main-branch run.

## Monthly

- **Bump pinned actions.** Search for `uses: .*@v\d+` in
  `.github/workflows/` and check the upstream repos for newer minor
  versions. Bump conservatively (don't jump major versions without
  reading the changelog).
- **Audit secrets.** In repo settings, confirm only these secrets are
  defined: `CACHIX_AUTH`, `CODECOV_TOKEN`, plus any deploy tokens.
  Remove anything else.
- **Garbage-collect Cachix.** `cache-warming.yml` keeps it warm but
  doesn't prune. Run a manual prune from Cachix dashboard if storage is
  approaching the plan limit.

## Quarterly

- **Re-derive cache key strategy.** Read `docs/CACHE_STRATEGY.md` and
  confirm the FLAKE_HASH/CARGO_HASH composition still matches what the
  workflows compute. Drift here causes silent cache misses.
- **Rotate `CACHIX_AUTH`.** Generate a new write-token in Cachix,
  update the GitHub secret, then revoke the old token.
- **Review branch-protection rules.** Confirm `All Checks Passed` and
  `guard` are still required on `main` and that no one has accidentally
  marked them as non-required.

## Ad-hoc

### Adding a new job to `ci.yml`
1. Add the job under `jobs:`.
2. Add its name to `jobs.all-checks.needs`. The aggregator will fail
   loudly if you forget — but it's faster to do it right the first time.
3. If the job is slow (>5 min) or requires special permissions, put it
   in its own workflow file instead.

### Adding a new workflow file
1. Decide if it should block merges. If yes, you need a way for branch
   protection to require it (a job name that always runs).
2. Set `permissions:` to the minimum it needs.
3. Add it to the table in [`architecture.md`](architecture.md).
4. If it consumes Cachix, set `skipPush:` for fork PRs (see existing
   workflows for the conditional).

### Responding to a CVE in a pinned action
1. Check the upstream issue tracker for a patched version.
2. Bump in all workflow files that use it.
3. If no patch exists, switch to an alternative or vendor the
   functionality. Don't ship a known-vulnerable workflow even
   short-term.

### Recovering from a poisoned Cachix entry
1. Identify the bad store path from build logs.
2. From Cachix dashboard, delete the path.
3. Re-run `cache-warming.yml` with `force_rebuild: true`.

## Health Metrics to Watch

Per issue #5, we should track:
- Build time trends (per-job, per-OS)
- Cache hit rate (Cachix dashboard)
- Failure rate by job
- Mean time to recovery (MTTR) for red `main`

`ci-metrics.yml` covers the first two via workflow-run summaries. The
other two require external aggregation — see issue #5 for the open
work.
