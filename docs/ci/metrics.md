# CI Metrics Workflow (issue #5)

This document is the minimum-viable seed for the first deliverable of
issue #5 — per-run CI performance metrics surfaced into the workflow
summary. The pattern mirrors [`integration-tests.md`](integration-tests.md):
a reviewer with `workflows` write scope can drop the YAML below in at
`.github/workflows/ci-metrics.yml` verbatim.

## Why a separate workflow

`ci.yml`'s `all-checks` job aggregates every other job's status and
*fails* if any job is missing from its `needs:` list. Adding a metrics
job to `ci.yml` would therefore put metrics collection in the critical
path of every PR. A separate workflow observing `ci.yml` via
`workflow_run` is decoupled: if metrics collection breaks, the product
gate is unaffected.

## What it collects

For each completed `ci.yml` run, the workflow writes a step-summary
table covering:

- Total wall-clock (from `created_at` → `updated_at`).
- Per-runner billable minutes (Ubuntu / macOS / Windows), via the
  `/actions/runs/{run_id}/timing` endpoint.
- Job outcome counts (succeeded / failed / cancelled / skipped).
- Per-job duration table (sorted by start time).

Cache hit/miss is stubbed. A real implementation requires fetching and
parsing job logs (looking for `cachix-action`'s "querying paths" vs
"building" markers); that's tracked as follow-up work in #5 alongside
the dashboard and alerting deliverables.

## Triggers

- `workflow_run` against `"CI/CD Pipeline"` with type `completed` —
  fires automatically after every `ci.yml` run.
- `workflow_dispatch` with a `run_id` input for manual back-fills.

Permissions: `contents: read`, `actions: read`. No write scope needed
since we only emit a step summary.

## Outstanding work tracked under #5

- Cache hit/miss extraction from job logs (`cachix-action` markers).
- Build-time trend chart (requires writing per-run metrics to a
  persistent store — GitHub Pages JSON, an issue body, or an external
  service).
- Failure-rate / MTTR monitoring (requires querying historical runs,
  not a single run).
- Resource utilization monitoring (the `/timing` endpoint gives
  billable minutes but not memory / CPU peaks; needs runner-side
  collection).
- Automated alerting on CI health regressions.
- Public CI health dashboard.

This first PR delivers the per-run summary table. The remainder is
follow-up work — see the PR comment thread on the metrics PR for the
status checklist.

## Reviewer action

Drop the following file at `.github/workflows/ci-metrics.yml`:

```yaml
name: CI Metrics

# Observes ci.yml completions and writes per-run performance metrics
# (build duration, cache hits/misses) to the workflow step summary.
#
# Runs out-of-band via workflow_run so it never participates in the
# critical path of ci.yml's `all-checks` gate (see docs/ci/architecture.md
# for the rationale). Tracks issue #5 — dashboard and alerting are
# follow-ups documented in that issue.

on:
  workflow_run:
    workflows: ["CI/CD Pipeline"]
    types: [completed]
  workflow_dispatch:
    inputs:
      run_id:
        description: "ci.yml run ID to analyze (defaults to latest completed run)"
        required: false
        default: ""

permissions:
  contents: read
  actions: read

jobs:
  collect-metrics:
    name: Collect CI metrics
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Resolve target run
        id: target
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          INPUT_RUN_ID: ${{ inputs.run_id }}
          EVENT_RUN_ID: ${{ github.event.workflow_run.id }}
        run: |
          set -euo pipefail
          if [ -n "${INPUT_RUN_ID}" ]; then
            run_id="${INPUT_RUN_ID}"
          elif [ -n "${EVENT_RUN_ID}" ]; then
            run_id="${EVENT_RUN_ID}"
          else
            echo "::error::no run_id available (manual dispatch without input and no workflow_run event)"
            exit 1
          fi
          echo "run_id=${run_id}" >> "$GITHUB_OUTPUT"
          echo "Target run: ${run_id}"

      - name: Fetch run timing and jobs
        id: fetch
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          RUN_ID: ${{ steps.target.outputs.run_id }}
          REPO: ${{ github.repository }}
        run: |
          set -euo pipefail
          api="https://api.github.com/repos/${REPO}"
          auth=(-H "Authorization: Bearer ${GH_TOKEN}" -H "Accept: application/vnd.github+json")

          curl -fsSL "${auth[@]}" "${api}/actions/runs/${RUN_ID}" > run.json
          curl -fsSL "${auth[@]}" "${api}/actions/runs/${RUN_ID}/timing" > timing.json
          curl -fsSL "${auth[@]}" "${api}/actions/runs/${RUN_ID}/jobs?per_page=100" > jobs.json

          conclusion=$(jq -r '.conclusion // "in_progress"' run.json)
          head_sha=$(jq -r '.head_sha' run.json)
          event=$(jq -r '.event' run.json)
          created_at=$(jq -r '.created_at' run.json)
          updated_at=$(jq -r '.updated_at' run.json)

          echo "conclusion=${conclusion}" >> "$GITHUB_OUTPUT"
          echo "head_sha=${head_sha}" >> "$GITHUB_OUTPUT"
          echo "event=${event}" >> "$GITHUB_OUTPUT"
          echo "created_at=${created_at}" >> "$GITHUB_OUTPUT"
          echo "updated_at=${updated_at}" >> "$GITHUB_OUTPUT"

      - name: Compute per-job durations and outcomes
        id: compute
        run: |
          set -euo pipefail

          jq -r '
            .jobs
            | map(select(.started_at != null and .completed_at != null))
            | sort_by(.started_at)
            | .[]
            | [
                .name,
                .status,
                .conclusion,
                ((( .completed_at | fromdateiso8601 ) - ( .started_at | fromdateiso8601 ))|tostring + "s")
              ]
            | @tsv
          ' jobs.json > job_durations.tsv

          total_jobs=$(jq '.jobs | length' jobs.json)
          succeeded=$(jq '[.jobs[] | select(.conclusion=="success")] | length' jobs.json)
          failed=$(jq '[.jobs[] | select(.conclusion=="failure")] | length' jobs.json)
          cancelled=$(jq '[.jobs[] | select(.conclusion=="cancelled")] | length' jobs.json)
          skipped=$(jq '[.jobs[] | select(.conclusion=="skipped")] | length' jobs.json)

          ubuntu_ms=$(jq '.billable.UBUNTU.total_ms // 0' timing.json)
          macos_ms=$(jq '.billable.MACOS.total_ms // 0' timing.json)
          windows_ms=$(jq '.billable.WINDOWS.total_ms // 0' timing.json)

          echo "total_jobs=${total_jobs}" >> "$GITHUB_OUTPUT"
          echo "succeeded=${succeeded}" >> "$GITHUB_OUTPUT"
          echo "failed=${failed}" >> "$GITHUB_OUTPUT"
          echo "cancelled=${cancelled}" >> "$GITHUB_OUTPUT"
          echo "skipped=${skipped}" >> "$GITHUB_OUTPUT"
          echo "ubuntu_ms=${ubuntu_ms}" >> "$GITHUB_OUTPUT"
          echo "macos_ms=${macos_ms}" >> "$GITHUB_OUTPUT"
          echo "windows_ms=${windows_ms}" >> "$GITHUB_OUTPUT"

      - name: Write summary
        env:
          RUN_ID: ${{ steps.target.outputs.run_id }}
          CONCLUSION: ${{ steps.fetch.outputs.conclusion }}
          HEAD_SHA: ${{ steps.fetch.outputs.head_sha }}
          EVENT: ${{ steps.fetch.outputs.event }}
          CREATED_AT: ${{ steps.fetch.outputs.created_at }}
          UPDATED_AT: ${{ steps.fetch.outputs.updated_at }}
          TOTAL_JOBS: ${{ steps.compute.outputs.total_jobs }}
          SUCCEEDED: ${{ steps.compute.outputs.succeeded }}
          FAILED: ${{ steps.compute.outputs.failed }}
          CANCELLED: ${{ steps.compute.outputs.cancelled }}
          SKIPPED: ${{ steps.compute.outputs.skipped }}
          UBUNTU_MS: ${{ steps.compute.outputs.ubuntu_ms }}
          MACOS_MS: ${{ steps.compute.outputs.macos_ms }}
          WINDOWS_MS: ${{ steps.compute.outputs.windows_ms }}
          REPO: ${{ github.repository }}
        run: |
          set -euo pipefail

          start_s=$(date -d "${CREATED_AT}" +%s)
          end_s=$(date -d "${UPDATED_AT}" +%s)
          wall_s=$(( end_s - start_s ))

          ubuntu_s=$(( UBUNTU_MS / 1000 ))
          macos_s=$(( MACOS_MS / 1000 ))
          windows_s=$(( WINDOWS_MS / 1000 ))
          billable_total_s=$(( ubuntu_s + macos_s + windows_s ))

          {
            echo "# CI Metrics — run ${RUN_ID}"
            echo
            echo "- Repo: \`${REPO}\`"
            echo "- Commit: \`${HEAD_SHA}\`"
            echo "- Trigger: \`${EVENT}\`"
            echo "- Conclusion: **${CONCLUSION}**"
            echo "- Run: https://github.com/${REPO}/actions/runs/${RUN_ID}"
            echo
            echo "## Wall-clock"
            echo
            echo "| metric | value |"
            echo "|--------|-------|"
            echo "| total wall-clock | ${wall_s}s |"
            echo "| billable (ubuntu) | ${ubuntu_s}s |"
            echo "| billable (macos) | ${macos_s}s |"
            echo "| billable (windows) | ${windows_s}s |"
            echo "| billable total | ${billable_total_s}s |"
            echo
            echo "## Job outcomes"
            echo
            echo "| outcome | count |"
            echo "|---------|-------|"
            echo "| total | ${TOTAL_JOBS} |"
            echo "| succeeded | ${SUCCEEDED} |"
            echo "| failed | ${FAILED} |"
            echo "| cancelled | ${CANCELLED} |"
            echo "| skipped | ${SKIPPED} |"
            echo
            echo "## Per-job duration"
            echo
            echo "| job | status | conclusion | duration |"
            echo "|-----|--------|------------|----------|"
            awk -F'\t' '{printf "| %s | %s | %s | %s |\n", $1, $2, $3, $4}' job_durations.tsv
            echo
            echo "_Cache hit/miss tracking is a stub. See issue #5 for the_"
            echo "_dashboard + log-parsing work that will populate it._"
          } >> "$GITHUB_STEP_SUMMARY"
```
