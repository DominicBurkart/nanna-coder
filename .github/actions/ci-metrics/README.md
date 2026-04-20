# ci-metrics composite action

Minimum-viable CI health monitoring primitive for the nanna-coder CI system.
Implements the foundation of issue #5 (CI health monitoring and performance
metrics) without introducing any external service dependency.

## What it does

- Captures per-job **start** / **end** timestamps and derives a duration.
- Records workflow / job / os / arch / ref / sha / event name.
- Accepts an optional `cache-hit` flag that callers pass through from
  whichever cache action they use (e.g. `actions/cache`, `cachix/cachix-action`).
- Writes a markdown summary to `$GITHUB_STEP_SUMMARY` that is visible on
  each run's summary page.
- Uploads the raw metric as a JSON artifact with 30-day retention so a
  later aggregator job / external tool can compute trends without needing
  history to be in the repo.

## What it explicitly does **not** do (follow-ups for #5)

- Trend dashboards / alerting.
- Regression detection (see issue #9).
- Resource (CPU / memory / disk I/O) sampling.
- Shipping metrics to an external monitoring service.
- Aggregating across runs — each run produces its own artifact.

## Usage

Call the action **twice** inside a job: once near the start, once at the end.

```yaml
jobs:
  unit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Record CI metrics (start)
        uses: ./.github/actions/ci-metrics
        with:
          phase: start
          metric-name: test-unit-linux

      # ... your real steps, e.g. cache restore, nix build, cargo nextest ...
      - name: Restore cache
        id: cache
        uses: actions/cache@v4
        with:
          path: ~/.cache/whatever
          key: unit-${{ hashFiles('Cargo.lock') }}

      - name: Run tests
        run: nix develop --command cargo nextest run --workspace --lib

      - name: Record CI metrics (end)
        if: always()
        uses: ./.github/actions/ci-metrics
        with:
          phase: end
          metric-name: test-unit-linux
          cache-hit: ${{ steps.cache.outputs.cache-hit }}
```

> **Important:** the `end` invocation should almost always be guarded by
> `if: always()` so that failed jobs still emit a metric (otherwise you
> systematically undercount failures when aggregating).

## Inputs

| name             | required | default               | description |
|------------------|----------|-----------------------|-------------|
| `phase`          | yes      | —                     | `start` or `end`. |
| `cache-hit`      | no       | `""`                  | Pass through from your cache action. Read only on `end`. |
| `metric-name`    | no       | `${{ github.job }}`   | Logical series name. Choose one per unique job × matrix cell. |
| `artifact-name`  | no       | derived               | Override for the uploaded artifact name. |
| `retention-days` | no       | `30`                  | Artifact retention. |

## Artifact schema (v1)

Each invocation produces a single JSON file like:

```json
{
  "schema_version": 1,
  "workflow": "CI/CD Pipeline",
  "job": "test-matrix",
  "metric_name": "test-unit-linux",
  "run_id": "...",
  "run_attempt": "1",
  "run_number": "...",
  "event_name": "pull_request",
  "ref": "refs/pull/123/merge",
  "sha": "...",
  "os": "Linux",
  "arch": "X64",
  "runner_name": "GitHub Actions 1",
  "start_ts": 1713600000,
  "end_ts": 1713600123,
  "duration_seconds": 123,
  "cache_hit": "true",
  "job_status": "success"
}
```

Consumers should treat unknown fields as additive; the `schema_version`
will be bumped if a field is removed or changes meaning.

## Why not wire it into `ci.yml` in this PR?

As of this PR, #243 is still open and is consolidating the shared setup
block in `ci.yml`, `cache-warming.yml`, and `eval.yml` into a single
composite action. To avoid a merge-conflict stampede, wiring this metrics
action into those workflows is deferred to a follow-up PR that lands
**after** #243 merges — at that point the wiring will happen in exactly
one place (the reusable setup composite) instead of three.
