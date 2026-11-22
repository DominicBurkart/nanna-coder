#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAUNCHER="$HERE/../../scripts/lumo-mcp.sh"
CONFIG_SRC="$HERE/../../scripts/lumo-config"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
FAILED=0

fail() { echo "FAIL: $1"; FAILED=1; }

mkdir -p "$WORK/bin"
cat > "$WORK/bin/podman" <<'FAKE'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FAKE_PODMAN_LOG"
case "${1:-} ${2:-}" in
  "volume exists") if [[ "${3:-}" == nanna-repo-* ]]; then [[ -e "$FAKE_VOLUME_DIR/$3" ]]; else [[ -e "$FAKE_VOLUME_FLAG" ]]; fi; exit $? ;;
  "volume rm") rm -f "$FAKE_VOLUME_FLAG"; exit 0 ;;
  "volume create") touch "$FAKE_VOLUME_FLAG"; exit 0 ;;
  "volume import") cat > "$FAKE_IMPORT_TAR"; exit 0 ;;
esac
printf '%s\n' "$@" > "$FAKE_PODMAN_ARGV"
cat
FAKE
chmod +x "$WORK/bin/podman"
export FAKE_PODMAN_ARGV="$WORK/argv" FAKE_PODMAN_LOG="$WORK/log" FAKE_VOLUME_FLAG="$WORK/volume" FAKE_IMPORT_TAR="$WORK/import.tar" FAKE_VOLUME_DIR="$WORK/volumes"
mkdir -p "$FAKE_VOLUME_DIR"
REPOS="marigold personal-site-actix nanna-coder one_track"
for r in $REPOS; do touch "$FAKE_VOLUME_DIR/nanna-repo-$r"; done
: > "$FAKE_PODMAN_LOG"

run_launcher() {
  env -i FAKE_PODMAN_ARGV="$FAKE_PODMAN_ARGV" FAKE_PODMAN_LOG="$FAKE_PODMAN_LOG" FAKE_VOLUME_FLAG="$FAKE_VOLUME_FLAG" \
    FAKE_IMPORT_TAR="$FAKE_IMPORT_TAR" FAKE_VOLUME_DIR="$FAKE_VOLUME_DIR" PATH="$WORK/bin:/usr/bin:/bin" HOME="$WORK/home" "$@"
}

touch "$FAKE_VOLUME_FLAG"
ping='{"jsonrpc":"2.0","id":1,"method":"ping"}'
run_launcher "$BASH" "$LAUNCHER" <<< "$ping" > "$WORK/out"
[[ "$(cat "$WORK/out")" == "$ping" ]] || fail "stdin must pass through to the container unchanged"

expected=$'run\n--rm\n-i\n--pod\nnanna-coder-pod\n-v\nnanna-lumo-config:/etc/nanna:ro\n-v\nnanna-repo-marigold:/repos/marigold\n-v\nnanna-repo-personal-site-actix:/repos/personal-site-actix\n-v\nnanna-repo-nanna-coder:/repos/nanna-coder\n-v\nnanna-repo-one_track:/repos/one_track\n-e\nNANNA_CONFIG_DIR=/etc/nanna\nghcr.io/dominicburkart/nanna-coder/harness:latest\n/bin/nanna\nmcp-serve\n--model\ngemma4:e4b'
[[ "$(cat "$FAKE_PODMAN_ARGV")" == "$expected" ]] || fail "default argv mismatch: $(cat "$FAKE_PODMAN_ARGV")"
! grep -q 'security-opt\|label=\|:z\|:Z' "$FAKE_PODMAN_ARGV" || fail "must never alter SELinux labelling or separation"
! grep -q 'privileged\|--userns\|--cap-add' "$FAKE_PODMAN_ARGV" || fail "must not widen container privileges"
mapfile -t vmounts < <(awk 'prev=="-v"{print} {prev=$0}' "$FAKE_PODMAN_ARGV")
[[ ${#vmounts[@]} -eq 5 ]] || fail "expected the config volume plus one volume per card repo, got ${#vmounts[@]}"
for v in "${vmounts[@]}"; do
  [[ "$v" =~ ^[A-Za-z0-9][A-Za-z0-9_.-]*:/[A-Za-z0-9_/.-]+(:ro)?$ ]] || fail "mount must be a named volume, got: $v"
  [[ "$v" != *'~'* && "$v" != *HOME* ]] || fail "mount must not reference the home directory: $v"
done
! grep -q "$WORK/home" "$FAKE_PODMAN_ARGV" || fail "must not mount any host path into the container"

run_launcher NANNA_MODEL=qwen3:0.6b NANNA_TAG=v1 NANNA_REGISTRY=reg.example/ns NANNA_POD=mypod NANNA_CONFIG_VOLUME=othervol \
  "$BASH" "$LAUNCHER" < /dev/null > /dev/null
touch "$FAKE_VOLUME_FLAG"
grep -qx 'qwen3:0.6b' "$FAKE_PODMAN_ARGV" || fail "NANNA_MODEL override ignored"
grep -qx 'reg.example/ns/harness:v1' "$FAKE_PODMAN_ARGV" || fail "NANNA_REGISTRY/NANNA_TAG override ignored"
grep -qx 'mypod' "$FAKE_PODMAN_ARGV" || fail "NANNA_POD override ignored"
grep -qx 'othervol:/etc/nanna:ro' "$FAKE_PODMAN_ARGV" || fail "NANNA_CONFIG_VOLUME override ignored"

rm -f "$FAKE_VOLUME_FLAG" "$FAKE_PODMAN_ARGV"
if run_launcher "$BASH" "$LAUNCHER" < /dev/null > /dev/null 2> "$WORK/err"; then
  fail "must fail closed when the capability config volume is missing"
else
  grep -q -- '--install-config' "$WORK/err" || fail "missing-volume error must point at --install-config"
fi
[[ ! -e "$FAKE_PODMAN_ARGV" ]] || fail "no container may start without the capability config"
touch "$FAKE_VOLUME_FLAG"

rm -f "$FAKE_VOLUME_DIR/nanna-repo-marigold"
if run_launcher "$BASH" "$LAUNCHER" < /dev/null > /dev/null 2> "$WORK/err"; then
  fail "must fail closed when a repo volume named by the card is missing"
else
  [[ $? -eq 3 ]] || fail "missing repo volume must exit 3"
  grep -q 'lumo-repos.sh' "$WORK/err" || fail "missing-repo error must point at lumo-repos.sh"
  grep -q 'nanna-repo-marigold' "$WORK/err" || fail "missing-repo error must name the volume"
fi
[[ ! -e "$FAKE_PODMAN_ARGV" ]] || fail "no container may start with a missing repo volume"

run_launcher NANNA_REPOS=nanna-coder,personal-site-actix "$BASH" "$LAUNCHER" < /dev/null > /dev/null
grep -qx 'nanna-repo-nanna-coder:/repos/nanna-coder' "$FAKE_PODMAN_ARGV" || fail "NANNA_REPOS must keep the named repos"
! grep -q 'marigold\|one_track' "$FAKE_PODMAN_ARGV" || fail "NANNA_REPOS must drop unnamed repos"
if run_launcher NANNA_REPOS=bogus "$BASH" "$LAUNCHER" < /dev/null > /dev/null 2>&1; then
  fail "NANNA_REPOS naming a repo outside the card must be rejected"
fi
touch "$FAKE_VOLUME_DIR/nanna-repo-marigold"

rm -f "$FAKE_PODMAN_ARGV"
if run_launcher NANNA_LUMO_CARD="$WORK/no-such-card.toml" "$BASH" "$LAUNCHER" < /dev/null > /dev/null 2>&1; then
  fail "an unreadable card must fail closed"
fi
[[ ! -e "$FAKE_PODMAN_ARGV" ]] || fail "no container may start when the repo list cannot be read"

: > "$FAKE_PODMAN_LOG"
run_launcher "$BASH" "$LAUNCHER" --install-config > /dev/null
[[ -e "$FAKE_VOLUME_FLAG" ]] || fail "--install-config must create the volume"
grep -q '^volume import nanna-lumo-config' "$FAKE_PODMAN_LOG" || fail "--install-config must import the config into the volume"
listing="$(tar -tf "$FAKE_IMPORT_TAR" | sed 's|^\./||' | grep -v '/$' | sort | tr '\n' ' ')"
[[ "$listing" == "agents/auditor.toml agents/lumo.toml agents/prompts/auditor.md agents/prompts/lumo.md " ]] \
  || fail "imported config must be exactly the auditor and lumo cards, got: $listing"

card="$CONFIG_SRC/agents/lumo.toml"
grep -q '^name = "lumo"$' "$card" || fail "lumo card must be named lumo"
grep -q '^max_effect = "workspace"$' "$card" || fail "lumo card must be capped at the workspace effect"
grep -q '^repos = \[".\+"\]$' "$card" || fail "lumo card must name its repositories explicitly"
! grep -q 'git_push\|git_commit\|github_\|deploy\|\*\*/\*\|"\*\*"' "$card" || fail "lumo card must not hold publish, network, deploy or wildcard capabilities"
grep -q '^max_concurrent = 1$' "$card" || fail "lumo card must run one task at a time"
grep -q '^max_effect = "none"$' "$CONFIG_SRC/agents/auditor.toml" || fail "auditor card must have no effect"

rm -f "$FAKE_PODMAN_ARGV"
out="$(run_launcher "$BASH" "$LAUNCHER" --print)"
grep -q '^name: nanna-coder$' <<< "$out" || fail "--print must show the server name"
grep -q '^command: /.*lumo-mcp.sh$' <<< "$out" || fail "--print must show the absolute launcher path as command"
[[ ! -e "$FAKE_PODMAN_ARGV" ]] || fail "--print must not start a container"

mkdir -p "$WORK/nopodman"
ln -s "$(command -v dirname)" "$WORK/nopodman/dirname"
ln -s "$(command -v basename)" "$WORK/nopodman/basename"
if env -i PATH="$WORK/nopodman" HOME="$WORK" "$BASH" "$LAUNCHER" < /dev/null > /dev/null 2> "$WORK/err"; then
  fail "must fail when podman is missing"
else
  grep -qi 'podman' "$WORK/err" || fail "missing-podman error must mention podman"
fi

if "$BASH" "$LAUNCHER" --bogus > /dev/null 2>&1; then fail "unknown flag must be rejected"; fi

if [[ $FAILED -eq 0 ]]; then echo "PASS: lumo-mcp launcher"; fi
exit $FAILED
