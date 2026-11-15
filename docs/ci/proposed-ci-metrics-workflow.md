# Proposed workflow: `ci-metrics.yml`

Minimum-viable slice for [issue #5](https://github.com/DominicBurkart/nanna-coder/issues/5)
(CI health monitoring & performance metrics).

## Why this lives in docs (for now)

The GitHub App used by the automated agent that authored this PR lacks the
`workflows` permission required to add a file under
`.github/workflows/`. Rather than block on that, the proposed workflow ships
here as a documented, review-ready YAML that a maintainer can install by
copying it into `.github/workflows/ci-metrics.yml` in a follow-up commit
(or one review-and-move commit on top of this PR).

## Scope of the slice

Deliberately narrow:

- Trigger on `workflow_run` completion of `CI/CD Pipeline` (also
  `workflow_dispatch` for backfill).
- Resolve the target run id.
- Fetch per-job timing via the GitHub Actions REST API (`/runs/:id/jobs`,
  paginated).
- Emit a step summary with an aggregate line and a per-job wall-time table
  sorted by duration.
- Upload raw `jobs.json` + `run.json` as a `ci-metrics-<run_id>` artifact
  (90-day retention).

Explicitly **out of scope** for this slice: dashboards, alerting, long-term
storage, regression detection, cache-hit metrics, resource-utilization
sampling. Those are tracked as follow-ups on issue #5.

## Ops footprint

- Uses only `actions/upload-artifact@v4`, `gh` CLI (preinstalled on
  ubuntu-latest runners), and `jq`.
- No new secrets.
- Permissions scoped to `contents: read` + `actions: read`.
- Concurrency group keyed by the analysed `run_id` so re-runs don't stomp.

## Installation

Copy the block below to `.github/workflows/ci-metrics.yml`. Commit and push.

## The workflow

```yaml
name: CI Metrics

# Collects per-job wall-time metrics from the primary CI pipeline and
# publishes them as a step summary + workflow artifact for trend analysis.
#
# This is the minimum-viable slice for issue #5 (CI health monitoring).
# It intentionally does not attempt dashboards, alerting, or long-term
# storage; those are tracked in follow-up sub-issues under #5.
#
# Docs: docs/ci/performance.md, docs/ci/architecture.md.

on:
  workflow_run:
    workflows: ["CI/CD Pipeline"]
    types: [completed]
  workflow_dispatch:
    inputs:
      run_id:
        description: "workflow_run ID to analyze (default: latest CI/CD Pipeline run on main)"
        required: false
        default: ""

permissions:
  contents: read
  actions: read

concurrency:
  group: ci-metrics-${{ github.event.workflow_run.id || github.event.inputs.run_id || github.ref }}
  cancel-in-progress: false

jobs:
  collect:
    name: Collect per-job build times
    runs-on: ubuntu-latest
    timeout-minutes: 5
    steps:
      - name: Resolve target run
        id: resolve
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          EVENT_RUN_ID: ${{ github.event.workflow_run.id }}
          INPUT_RUN_ID: ${{ github.event.inputs.run_id }}
          REPO: ${{ github.repository }}
        run: |
          set -euo pipefail

          if [ -n "${EVENT_RUN_ID:-}" ]; then
            RUN_ID="$EVENT_RUN_ID"
            echo "Source: workflow_run event"
          elif [ -n "${INPUT_RUN_ID:-}" ]; then
            RUN_ID="$INPUT_RUN_ID"
            echo "Source: workflow_dispatch input"
          else
            echo "Source: latest completed CI/CD Pipeline run on main"
            RUN_ID=$(gh run list \
              --repo "$REPO" \
              --workflow "CI/CD Pipeline" \
              --branch main \
              --status completed \
              --limit 1 \
              --json databaseId \
              --jq '.[0].databaseId')
          fi

          if [ -z "${RUN_ID:-}" ] || [ "$RUN_ID" = "null" ]; then
            echo "::error::Could not resolve a target workflow run id"
            exit 1
          fi

          echo "run_id=$RUN_ID" >> "$GITHUB_OUTPUT"
          echo "Target run id: $RUN_ID"

      - name: Fetch job timing data
        id: fetch
        env:
          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          REPO: ${{ github.repository }}
          RUN_ID: ${{ steps.resolve.outputs.run_id }}
        run: |
          set -euo pipefail

          mkdir -p metrics

          # Pull all jobs for the target run. Pagination is via --paginate.
          gh api \
            --paginate \
            -H "Accept: application/vnd.github+json" \
            "/repos/$REPO/actions/runs/$RUN_ID/jobs" \
            > metrics/raw-jobs.json

          # Extract per-job (name, conclusion, started_at, completed_at,
          # duration_seconds) into a flat JSON array.
          jq '
            [ .jobs[] |
              select(.started_at != null and .completed_at != null) |
              {
                name: .name,
                conclusion: .conclusion,
                status: .status,
                started_at: .started_at,
                completed_at: .completed_at,
                duration_seconds: ((.completed_at | fromdateiso8601) - (.started_at | fromdateiso8601)),
                runner_name: .runner_name,
                labels: .labels
              }
            ]
          ' metrics/raw-jobs.json > metrics/jobs.json

          # Also emit workflow-level summary.
          gh api \
            -H "Accept: application/vnd.github+json" \
            "/repos/$REPO/actions/runs/$RUN_ID" \
            | jq '{
                run_id: .id,
                name: .name,
                display_title: .display_title,
                head_branch: .head_branch,
                head_sha: .head_sha,
                event: .event,
                status: .status,
                conclusion: .conclusion,
                run_attempt: .run_attempt,
                run_started_at: .run_started_at,
                updated_at: .updated_at
              }' \
            > metrics/run.json

          # Persist run id for downstream steps.
          jq -c . metrics/run.json

      - name: Write step summary
        env:
          RUN_ID: ${{ steps.resolve.outputs.run_id }}
          REPO: ${{ github.repository }}
        run: |
          set -euo pipefail

          RUN_URL="https://github.com/$REPO/actions/runs/$RUN_ID"

          {
            echo "# CI wall-time report"
            echo ""
            echo "Analysed run: [$RUN_ID]($RUN_URL)"
            echo ""

            HEAD_SHA=$(jq -r '.head_sha' metrics/run.json)
            HEAD_BRANCH=$(jq -r '.head_branch' metrics/run.json)
            CONCLUSION=$(jq -r '.conclusion // "in_progress"' metrics/run.json)
            ATTEMPT=$(jq -r '.run_attempt' metrics/run.json)

            echo "- Branch: \`$HEAD_BRANCH\`"
            echo "- Commit: \`$HEAD_SHA\`"
            echo "- Attempt: $ATTEMPT"
            echo "- Conclusion: $CONCLUSION"
            echo ""

            TOTAL=$(jq '[.[].duration_seconds] | add // 0 | floor' metrics/jobs.json)
            LONGEST=$(jq -r 'sort_by(-.duration_seconds) | .[0] // {} | "\(.name // "n/a") — \(.duration_seconds // 0 | floor)s"' metrics/jobs.json)
            COUNT=$(jq 'length' metrics/jobs.json)

            echo "## Aggregate"
            echo ""
            echo "| Metric | Value |"
            echo "|--------|-------|"
            echo "| Jobs analysed | $COUNT |"
            echo "| Total wall time (sum) | ${TOTAL}s |"
            echo "| Longest job | $LONGEST |"
            echo ""

            echo "## Per-job wall time"
            echo ""
            echo "| Job | Duration (s) | Conclusion |"
            echo "|-----|--------------|------------|"

            jq -r 'sort_by(-.duration_seconds) | .[] |
              "| \(.name) | \(.duration_seconds | floor) | \(.conclusion // "n/a") |"' \
              metrics/jobs.json
          } >> "$GITHUB_STEP_SUMMARY"

      - name: Upload metrics artifact
        uses: actions/upload-artifact@v4
        with:
          name: ci-metrics-${{ steps.resolve.outputs.run_id }}
          path: metrics/
          if-no-files-found: error
          retention-days: 90
```

## Follow-ups after install

- Add the workflow to the inventory table in
  [`docs/ci/architecture.md`](architecture.md#workflow-inventory) (row is
  already present, pointing at the installed path).
- Confirm the first `workflow_run`-triggered run produces both the step
  summary and the artifact.
- Once a few `main`-branch runs have accumulated, sketch out the trend-line
  follow-up as a sub-issue of #5 (see the plan comment on #5 for the full
  decomposition).
