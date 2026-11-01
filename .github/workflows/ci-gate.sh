#!/usr/bin/env bash
set -euo pipefail

allowed_file="${ALLOWED_SKIPS_FILE:-$(dirname "${BASH_SOURCE[0]}")/ci-gate-allowed-skips.txt}"
required_jobs='["test-matrix","build-matrix","build-containers"]'
failures=""

add_failure() {
  failures+="$1"$'\n'
}

: "${NEEDS_JSON:?NEEDS_JSON is required}"
: "${GATE_EVENT:?GATE_EVENT is required}"
: "${GATE_REF:?GATE_REF is required}"

if ! printf '%s' "$NEEDS_JSON" | jq -e '
    type == "object" and length > 0
    and all(.[]; type == "object" and (.result | type) == "string")
  ' >/dev/null 2>&1; then
  echo "::error::needs context is empty or malformed"
  exit 1
fi

while IFS= read -r job; do
  if ! printf '%s' "$NEEDS_JSON" | jq -e --arg j "$job" 'has($j)' >/dev/null; then
    add_failure "required job '$job' is absent from the needs context"
  fi
done < <(printf '%s' "$required_jobs" | jq -r '.[]')

skip_allowed() {
  local job="$1" event ref
  [ -f "$allowed_file" ] || return 1
  while read -r a_job a_event a_ref; do
    [ -n "${a_job:-}" ] || continue
    [ "$a_job" = "$job" ] || continue
    [ "$a_event" = "$GATE_EVENT" ] || continue
    if [ "$a_ref" = "*" ] || [ "$a_ref" = "$GATE_REF" ]; then
      return 0
    fi
  done < "$allowed_file"
  return 1
}

needs_rows=$(printf '%s' "$NEEDS_JSON" | jq -r 'to_entries[] | [.key, .value.result] | @tsv')

while IFS=$'\t' read -r job result; do
  case "$result" in
    success) ;;
    skipped)
      if ! skip_allowed "$job"; then
        add_failure "job '$job' was skipped, which is not allowed for event '$GATE_EVENT' on '$GATE_REF'"
      fi
      ;;
    *)
      add_failure "job '$job' ended with result '$result'"
      ;;
  esac
done <<< "$needs_rows"

if [ -n "${JOBS_JSON_FILE:-}" ]; then
  self="${GATE_SELF_NAME:-All Checks Passed}"
  if ! jq -e '
      type == "object" and (.jobs | type) == "array" and (.jobs | length) > 0
      and all(.jobs[]; (.name | type) == "string" and (.status | type) == "string")
    ' "$JOBS_JSON_FILE" >/dev/null 2>&1; then
    add_failure "per-job listing is missing, empty or malformed"
  else
    while IFS=$'\t' read -r name status conclusion; do
      if [ "$status" != "completed" ]; then
        add_failure "job '$name' is not completed (status '$status')"
      elif [ "$conclusion" != "success" ] && [ "$conclusion" != "skipped" ]; then
        add_failure "job '$name' concluded '$conclusion'"
      fi
    done < <(jq -r --arg self "$self" \
      '.jobs[] | select(.name != $self) | [.name, .status, (.conclusion // "null")] | @tsv' \
      "$JOBS_JSON_FILE")
  fi
fi

if [ -n "$failures" ]; then
  while IFS= read -r line; do
    [ -n "$line" ] && echo "::error::$line"
  done <<< "$failures"
  exit 1
fi

echo "OK: every needed job succeeded or was skipped by an allowed rule"
