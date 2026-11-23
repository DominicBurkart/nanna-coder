#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

if ! command -v python3 > /dev/null 2>&1; then
  echo "FAIL: python3 is required to test the bridge"
  exit 1
fi

exec python3 -I "$HERE/lumo_bridge_test.py" "$@"
