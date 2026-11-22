#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LAUNCHER="$HERE/../../scripts/lumo-mcp.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
FAILED=0

fail() { echo "FAIL: $1"; FAILED=1; }

mkdir -p "$WORK/bin" "$WORK/state/volumes"
cat > "$WORK/bin/podman" <<'FAKE'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FAKE_PODMAN_LOG"
S="$FAKE_STATE"
case "${1:-} ${2:-}" in
  "volume exists") [[ -e "$S/volumes/$3" ]]; exit $? ;;
  "volume create") touch "$S/volumes/$3"; exit 0 ;;
  "volume rm") rm -f "$S/volumes/${@: -1}"; exit 0 ;;
  "volume import") cat > "$FAKE_IMPORT_TAR"; exit 0 ;;
  "container exists") [[ -e "$S/container" ]]; exit $? ;;
  "image inspect") printf '%s\n' "$FAKE_IMAGE_ID"; exit 0 ;;
esac
case "${1:-}" in
  inspect)
    [[ -e "$S/container" ]] || exit 125
    case "$*" in
      *State.Running*) cat "$S/running" ;;
      *'{{.Image}}'*) cat "$S/image" ;;
    esac
    exit 0
    ;;
  run)
    if [[ -e "$S/container" ]]; then echo "Error: the container name is already in use" >&2; exit 125; fi
    printf '%s\n' "$@" > "$FAKE_RUN_ARGV"
    if [[ -n "${FAKE_RUN_FAILS:-}" ]]; then exit 126; fi
    touch "$S/container"; echo true > "$S/running"; echo "$FAKE_IMAGE_ID_RUNNING" > "$S/image"
    if [[ -n "${FAKE_RACE:-}" ]]; then echo "Error: the container name is already in use" >&2; exit 125; fi
    echo fakecontainerid
    exit 0
    ;;
  start) echo true > "$S/running"; exit 0 ;;
  stop) echo false > "$S/running"; exit 0 ;;
  rm) rm -f "$S/container" "$S/running" "$S/image" "$S/polls"; exit 0 ;;
  logs) echo "fake daemon log line"; exit 0 ;;
  exec)
    shift
    [[ "${1:-}" == "-i" ]] && shift
    shift
    if [[ "${1:-}" == "test" ]]; then
      n=$(( $(cat "$S/polls" 2> /dev/null || echo 0) + 1 )); echo "$n" > "$S/polls"
      [[ -z "${FAKE_NO_SOCKET:-}" && "$n" -gt "${FAKE_SOCKET_AFTER:-0}" ]]; exit $?
    fi
    if [[ "${1:-}" == "git" ]]; then exit "${FAKE_GIT_EXIT:-0}"; fi
    printf '%s\n' "$@" >> "$FAKE_ATTACH_ARGV"
    if [[ "$*" == *"--client status"* ]]; then
      cat > /dev/null
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"tasks":[{"taskId":"a","status":"working"},{"taskId":"b","status":"completed"},{"taskId":"c","status":"completed"}]}}'
      exit 0
    fi
    cat
    exit "${FAKE_ATTACH_EXIT:-0}"
    ;;
esac
exit 0
FAKE
chmod +x "$WORK/bin/podman"
export FAKE_STATE="$WORK/state" FAKE_RUN_ARGV="$WORK/run.argv" FAKE_ATTACH_ARGV="$WORK/attach.argv" FAKE_PODMAN_LOG="$WORK/log" \
  FAKE_IMPORT_TAR="$WORK/import.tar" FAKE_IMAGE_ID=sha256:current FAKE_IMAGE_ID_RUNNING=sha256:current

CATALOG="$WORK/catalog"
mkdir -p "$CATALOG/prompts"
for card in auditor rust-implementer pr-shepherd reviewer docs-writer; do
  printf '[identity]\nname = "%s"\n\n[scope]\nrepos = ["github.com/DominicBurkart/marigold", "github.com/DominicBurkart/personal-site-actix", "github.com/DominicBurkart/nanna-coder", "github.com/1-Track/one_track"]\n' "$card" > "$CATALOG/$card.toml"
  printf 'prompt for %s\n' "$card" > "$CATALOG/prompts/$card.md"
done

REPOS="marigold personal-site-actix nanna-coder one_track"
reset_world() {
  rm -rf "$WORK/state"; mkdir -p "$WORK/state/volumes"
  touch "$WORK/state/volumes/nanna-lumo-config"
  for r in $REPOS; do touch "$WORK/state/volumes/nanna-repo-$r"; done
  rm -f "$FAKE_RUN_ARGV" "$FAKE_ATTACH_ARGV" "$FAKE_IMPORT_TAR"
  : > "$FAKE_PODMAN_LOG"
}

run_launcher() {
  local -a passthrough=()
  local v
  for v in FAKE_RACE FAKE_RUN_FAILS FAKE_SOCKET_AFTER FAKE_NO_SOCKET FAKE_ATTACH_EXIT FAKE_GIT_EXIT; do
    if [[ -n "${!v:-}" ]]; then passthrough+=("$v=${!v}"); fi
  done
  env -i "${passthrough[@]}" FAKE_STATE="$FAKE_STATE" FAKE_RUN_ARGV="$FAKE_RUN_ARGV" FAKE_ATTACH_ARGV="$FAKE_ATTACH_ARGV" FAKE_PODMAN_LOG="$FAKE_PODMAN_LOG" \
    FAKE_IMPORT_TAR="$FAKE_IMPORT_TAR" FAKE_IMAGE_ID="$FAKE_IMAGE_ID" FAKE_IMAGE_ID_RUNNING="$FAKE_IMAGE_ID_RUNNING" \
    NANNA_CATALOG_SRC="$CATALOG" NANNA_DAEMON_WAIT_SECS=2 PATH="$WORK/bin:/usr/bin:/bin" HOME="$WORK/home" "$@"
}

ping='{"jsonrpc":"2.0","id":1,"method":"ping"}'
BASEURL=ghcr.io/dominicburkart/nanna-coder/harness:latest

reset_world
run_launcher "$BASH" "$LAUNCHER" <<< "$ping" > "$WORK/out" 2> "$WORK/err" || fail "default launch must succeed: $(cat "$WORK/err")"
[[ "$(cat "$WORK/out")" == "$ping" ]] || fail "stdin must pass through the bridge to the container unchanged, and stdout carry nothing else"

expected=$'run\n-d\n--name\nnanna-daemon\n--pod\nnanna-coder-pod\n--restart=no\n-v\nnanna-state:/state\n-v\nnanna-lumo-config:/etc/nanna:ro\n-v\nnanna-repo-marigold:/repos/marigold\n-v\nnanna-repo-personal-site-actix:/repos/personal-site-actix\n-v\nnanna-repo-nanna-coder:/repos/nanna-coder\n-v\nnanna-repo-one_track:/repos/one_track\n-e\nNANNA_CONFIG_DIR=/etc/nanna\n-e\nNANNA_QUEUE_PATH=/state/queue.jsonl\n-e\nNANNA_STATE_DIR=/state\n-e\nNANNA_AUDIT_TIMEOUT_SECS=300\n'"$BASEURL"$'\n/bin/nanna\nmcp-serve\n--listen\n/run/nanna/mcp.sock\n--model\ngemma4:e4b'
[[ "$(cat "$FAKE_RUN_ARGV")" == "$expected" ]] || fail "daemon argv mismatch: $(cat "$FAKE_RUN_ARGV")"
expected_attach=$'/bin/nanna\nmcp-attach\n--client\nlumo'
[[ "$(cat "$FAKE_ATTACH_ARGV")" == "$expected_attach" ]] || fail "the bridge must run nanna mcp-attach --client lumo, got: $(cat "$FAKE_ATTACH_ARGV")"
grep -qx 'exec -i nanna-daemon /bin/nanna mcp-attach --client lumo' "$FAKE_PODMAN_LOG" || fail "the bridge must be podman exec -i into the daemon"
for r in $REPOS; do
  grep -qx "exec nanna-daemon git -C /repos/$r worktree prune" "$FAKE_PODMAN_LOG" || fail "daemon start must prune worktree records of $r"
done
! grep -q 'security-opt\|label=\|:z\|:Z' "$FAKE_RUN_ARGV" "$FAKE_PODMAN_LOG" || fail "must never alter SELinux labelling or separation"
! grep -q 'privileged\|--userns\|--cap-add\|--publish\|^-p$\|--network\|--net\b' "$FAKE_RUN_ARGV" || fail "must not widen privileges or publish ports"
mapfile -t vmounts < <(awk 'prev=="-v"{print} {prev=$0}' "$FAKE_RUN_ARGV")
[[ ${#vmounts[@]} -eq 6 ]] || fail "expected state, config and four repo volumes, got ${#vmounts[@]}"
for v in "${vmounts[@]}"; do
  [[ "$v" =~ ^[A-Za-z0-9][A-Za-z0-9_.-]*:/[A-Za-z0-9_/.-]+(:ro)?$ ]] || fail "mount must be a named volume, got: $v"
  [[ "$v" != *'~'* && "$v" != *HOME* ]] || fail "mount must not reference the home directory: $v"
done
grep -qx 'nanna-lumo-config:/etc/nanna:ro' "$FAKE_RUN_ARGV" || fail "the config volume must be read-only"
grep -qx 'nanna-state:/state' "$FAKE_RUN_ARGV" || fail "the state volume must be mounted read-write at /state"
! grep -q "$WORK/home\|$CATALOG" "$FAKE_RUN_ARGV" || fail "must not mount any host path into the container"
[[ "$(awk '$1=="run"' "$FAKE_PODMAN_LOG" | wc -l)" -eq 1 ]] || fail "exactly one container must be created"

reset_world
touch "$FAKE_STATE/container"; echo true > "$FAKE_STATE/running"; echo sha256:old > "$FAKE_STATE/image"
run_launcher "$BASH" "$LAUNCHER" <<< "$ping" > "$WORK/out" 2> /dev/null || fail "attaching to a running daemon must succeed"
! grep -qE '^(run|start|stop|rm) ' "$FAKE_PODMAN_LOG" || fail "a running daemon (even on a stale image) must never be restarted, replaced or removed"
[[ "$(cat "$WORK/out")" == "$ping" ]] || fail "attaching to a running daemon must pass stdin through"

reset_world
touch "$FAKE_STATE/container"; echo false > "$FAKE_STATE/running"; echo sha256:current > "$FAKE_STATE/image"
run_launcher "$BASH" "$LAUNCHER" <<< "$ping" > "$WORK/out" 2> /dev/null || fail "a stopped daemon must be restarted"
grep -qx 'start nanna-daemon' "$FAKE_PODMAN_LOG" || fail "a stopped daemon must be started with podman start"
! grep -qE '^(run|rm) ' "$FAKE_PODMAN_LOG" || fail "a stopped daemon must not be recreated"
grep -qx 'exec nanna-daemon git -C /repos/marigold worktree prune' "$FAKE_PODMAN_LOG" || fail "starting a stopped daemon must prune worktree records too"

reset_world
FAKE_RACE=1 run_launcher "$BASH" "$LAUNCHER" <<< "$ping" > "$WORK/out" 2> "$WORK/err" || fail "losing the creation race must fall back to the winner's daemon: $(cat "$WORK/err")"
[[ "$(cat "$WORK/out")" == "$ping" ]] || fail "the race loser must still attach"

reset_world
FAKE_RUN_FAILS=1 run_launcher "$BASH" "$LAUNCHER" < /dev/null > /dev/null 2> "$WORK/err" && fail "a failed creation without a surviving container must fail"
[[ ! -e "$FAKE_ATTACH_ARGV" ]] || fail "no attach after a failed creation"

reset_world
FAKE_SOCKET_AFTER=3 run_launcher "$BASH" "$LAUNCHER" <<< "$ping" > "$WORK/out" 2> /dev/null || fail "a socket that appears late must be waited for"
[[ "$(cat "$WORK/out")" == "$ping" ]] || fail "attach after waiting must pass stdin through"

reset_world
if FAKE_NO_SOCKET=1 run_launcher "$BASH" "$LAUNCHER" < /dev/null > "$WORK/out" 2> "$WORK/err"; then
  fail "a daemon that never opens its socket must fail the launch"
else
  [[ $? -eq 4 ]] || fail "a socket timeout must exit 4"
  grep -q 'did not open' "$WORK/err" || fail "the timeout must be explained on stderr"
fi
[[ ! -s "$WORK/out" ]] || fail "failures must not write to stdout"
[[ ! -e "$FAKE_ATTACH_ARGV" ]] || fail "never attach without a socket"

reset_world
FAKE_ATTACH_EXIT=7 run_launcher "$BASH" "$LAUNCHER" <<< "$ping" > /dev/null 2>&1 && fail "a bridge that dies must make the launcher exit non-zero"
code=$?
[[ $code -ne 0 ]] || fail "a dying daemon must surface as a non-zero exit so Lumo reconnects"

reset_world
run_launcher NANNA_MODEL=qwen3:0.6b NANNA_TAG=v1 NANNA_REGISTRY=reg.example/ns NANNA_POD=mypod NANNA_CONFIG_VOLUME=othervol \
  NANNA_AUDIT_TIMEOUT_SECS=45 NANNA_STATE_VOLUME=otherstate NANNA_RESULT_RETENTION_DAYS=3 "$BASH" "$LAUNCHER" < /dev/null > /dev/null 2>&1 && fail "an override to a config volume that does not exist must fail closed"
touch "$FAKE_STATE/volumes/othervol"
run_launcher NANNA_MODEL=qwen3:0.6b NANNA_TAG=v1 NANNA_REGISTRY=reg.example/ns NANNA_POD=mypod NANNA_CONFIG_VOLUME=othervol \
  NANNA_AUDIT_TIMEOUT_SECS=45 NANNA_STATE_VOLUME=otherstate NANNA_RESULT_RETENTION_DAYS=3 "$BASH" "$LAUNCHER" < /dev/null > /dev/null 2>&1
grep -qx 'qwen3:0.6b' "$FAKE_RUN_ARGV" || fail "NANNA_MODEL override ignored"
grep -qx 'reg.example/ns/harness:v1' "$FAKE_RUN_ARGV" || fail "NANNA_REGISTRY/NANNA_TAG override ignored"
grep -qx 'mypod' "$FAKE_RUN_ARGV" || fail "NANNA_POD override ignored"
grep -qx 'othervol:/etc/nanna:ro' "$FAKE_RUN_ARGV" || fail "NANNA_CONFIG_VOLUME override ignored"
grep -qx 'otherstate:/state' "$FAKE_RUN_ARGV" || fail "NANNA_STATE_VOLUME override ignored"
grep -qx 'NANNA_AUDIT_TIMEOUT_SECS=45' "$FAKE_RUN_ARGV" || fail "NANNA_AUDIT_TIMEOUT_SECS override ignored"
grep -qx 'NANNA_RESULT_RETENTION_DAYS=3' "$FAKE_RUN_ARGV" || fail "NANNA_RESULT_RETENTION_DAYS must be passed through when set"

reset_world
rm -f "$FAKE_STATE/volumes/nanna-lumo-config"
if run_launcher "$BASH" "$LAUNCHER" < /dev/null > /dev/null 2> "$WORK/err"; then
  fail "must fail closed when the identity catalog volume is missing"
else
  [[ $? -eq 3 ]] || fail "a missing catalog volume must exit 3"
  grep -q -- '--install-catalog' "$WORK/err" || fail "missing-volume error must point at --install-catalog"
fi
[[ ! -e "$FAKE_RUN_ARGV" ]] || fail "no container may start without the identity catalog"

reset_world
rm -f "$FAKE_STATE/volumes/nanna-repo-marigold"
if run_launcher "$BASH" "$LAUNCHER" < /dev/null > /dev/null 2> "$WORK/err"; then
  fail "must fail closed when a repo volume is missing"
else
  [[ $? -eq 3 ]] || fail "missing repo volume must exit 3"
  grep -q 'lumo-repos.sh' "$WORK/err" || fail "missing-repo error must point at lumo-repos.sh"
  grep -q 'nanna-repo-marigold' "$WORK/err" || fail "missing-repo error must name the volume"
fi
[[ ! -e "$FAKE_RUN_ARGV" ]] || fail "no container may start with a missing repo volume"

reset_world
run_launcher NANNA_REPOS=nanna-coder,personal-site-actix "$BASH" "$LAUNCHER" < /dev/null > /dev/null 2>&1
grep -qx 'nanna-repo-nanna-coder:/repos/nanna-coder' "$FAKE_RUN_ARGV" || fail "NANNA_REPOS must keep the named repos"
! grep -q 'marigold\|one_track' "$FAKE_RUN_ARGV" || fail "NANNA_REPOS must drop unnamed repos"
reset_world
if run_launcher NANNA_REPOS=bogus "$BASH" "$LAUNCHER" < /dev/null > /dev/null 2>&1; then
  fail "NANNA_REPOS naming a repo outside the catalog must be rejected"
fi

reset_world
if run_launcher NANNA_CATALOG_SRC="$WORK/no-such-catalog" "$BASH" "$LAUNCHER" < /dev/null > /dev/null 2>&1; then
  fail "an unreadable catalog must fail closed"
fi
[[ ! -e "$FAKE_RUN_ARGV" ]] || fail "no container may start when the repo list cannot be read"

reset_world
touch "$FAKE_STATE/container"; echo true > "$FAKE_STATE/running"; echo sha256:current > "$FAKE_STATE/image"
run_launcher "$BASH" "$LAUNCHER" --stop > "$WORK/out" 2> "$WORK/err" || fail "--stop must succeed"
grep -qx 'stop nanna-daemon' "$FAKE_PODMAN_LOG" || fail "--stop must podman stop the daemon"
grep -qx 'rm nanna-daemon' "$FAKE_PODMAN_LOG" || fail "--stop must remove the container"
! grep -q 'volume rm' "$FAKE_PODMAN_LOG" || fail "--stop must keep every volume, the state volume above all"
[[ ! -e "$FAKE_STATE/container" ]] || fail "--stop must leave no container"
run_launcher "$BASH" "$LAUNCHER" --stop > /dev/null 2>&1 || fail "--stop with no daemon must be a no-op"

reset_world
touch "$FAKE_STATE/container"; echo true > "$FAKE_STATE/running"; echo sha256:old > "$FAKE_STATE/image"
run_launcher "$BASH" "$LAUNCHER" --restart > /dev/null 2>&1 || fail "--restart must succeed"
lines="$(grep -nE '^(stop|rm|run) ' "$FAKE_PODMAN_LOG" | cut -d: -f2 | cut -d' ' -f1 | tr '\n' ' ')"
[[ "$lines" == "stop rm run " ]] || fail "--restart must stop, remove then run, got: $lines"
! grep -q 'volume rm' "$FAKE_PODMAN_LOG" || fail "--restart must keep the volumes"

reset_world
run_launcher "$BASH" "$LAUNCHER" --start > "$WORK/out" 2> /dev/null || fail "--start must succeed"
[[ ! -s "$WORK/out" ]] || fail "--start must not write to stdout"
[[ ! -e "$FAKE_ATTACH_ARGV" ]] || fail "--start must not attach"
[[ -e "$FAKE_RUN_ARGV" ]] || fail "--start must create the daemon"

reset_world
touch "$FAKE_STATE/container"; echo true > "$FAKE_STATE/running"; echo sha256:current > "$FAKE_STATE/image"
run_launcher "$BASH" "$LAUNCHER" --status > "$WORK/out" 2> /dev/null || fail "--status of a running daemon must exit 0"
grep -q '^daemon: running' "$WORK/out" || fail "--status must report running: $(cat "$WORK/out")"
grep -q 'image: .*(current)' "$WORK/out" || fail "--status must report a current image"
grep -q '^socket: present' "$WORK/out" || fail "--status must report the socket"
grep -q 'tasks: .*completed=2' "$WORK/out" && grep -q 'working=1' "$WORK/out" || fail "--status must report task counts from a round trip: $(cat "$WORK/out")"
! grep -qE '^(run|start|stop|rm) ' "$FAKE_PODMAN_LOG" || fail "--status must change nothing"
echo sha256:old > "$FAKE_STATE/image"
run_launcher "$BASH" "$LAUNCHER" --status > "$WORK/out" 2> /dev/null || fail "--status of a stale daemon must still exit 0"
grep -q 'STALE' "$WORK/out" || fail "--status must flag a stale image"
! grep -qE '^(run|start|stop|rm) ' "$FAKE_PODMAN_LOG" || fail "a stale image must be reported, never replaced"
echo false > "$FAKE_STATE/running"
if run_launcher "$BASH" "$LAUNCHER" --status > "$WORK/out" 2> /dev/null; then fail "--status of a stopped daemon must exit non-zero"; fi
grep -q '^daemon: stopped' "$WORK/out" || fail "--status must report stopped"
rm -f "$FAKE_STATE/container"
if run_launcher "$BASH" "$LAUNCHER" --status > "$WORK/out" 2> /dev/null; then fail "--status of an absent daemon must exit non-zero"; fi
grep -q '^daemon: absent' "$WORK/out" || fail "--status must report absent"
! grep -qE '^(run|start) ' "$FAKE_PODMAN_LOG" || fail "--status must never start a daemon"

reset_world
run_launcher "$BASH" "$LAUNCHER" --install-catalog > "$WORK/out" 2> "$WORK/err" || fail "--install-catalog must succeed: $(cat "$WORK/err")"
[[ -e "$FAKE_STATE/volumes/nanna-lumo-config" ]] || fail "--install-catalog must create the volume"
grep -q '^volume import nanna-lumo-config' "$FAKE_PODMAN_LOG" || fail "--install-catalog must import into the config volume"
listing="$(tar -tf "$FAKE_IMPORT_TAR" | sed 's|^\./||' | grep -v '/$' | sort | tr '\n' ' ')"
want="$(printf '%s\n' agents/auditor.toml agents/docs-writer.toml agents/pr-shepherd.toml agents/reviewer.toml agents/rust-implementer.toml agents/prompts/auditor.md agents/prompts/docs-writer.md agents/prompts/pr-shepherd.md agents/prompts/reviewer.md agents/prompts/rust-implementer.md | sort | tr '\n' ' ')"
[[ "$listing" == "$want" ]] || fail "imported catalog must be exactly the source files, got: $listing"
! tar -tf "$FAKE_IMPORT_TAR" | grep -q lumo || fail "no lumo card may exist in the catalog"
for id in auditor docs-writer pr-shepherd reviewer rust-implementer; do
  grep -q "$id" "$WORK/out" || fail "--install-catalog must print the installed identity $id"
done
! grep '^identities:' "$WORK/out" | grep -q 'lumo' || fail "--install-catalog must not list a lumo identity"
! grep -q 'security-opt\|label=\|:z\|:Z\|privileged\|-v /' "$FAKE_PODMAN_LOG" || fail "--install-catalog must not relabel or bind-mount"

reset_world
mkdir -p "$WORK/real" "$WORK/linked/prompts"
cp "$CATALOG/auditor.toml" "$WORK/real/auditor.toml"
ln -s "$WORK/real/auditor.toml" "$WORK/linked/auditor.toml"
printf 'x\n' > "$WORK/linked/prompts/auditor.md"
run_launcher NANNA_CATALOG_SRC="$WORK/linked" "$BASH" "$LAUNCHER" --install-catalog > /dev/null 2>&1 || fail "a catalog of symlinks must install"
[[ "$(tar -tvf "$FAKE_IMPORT_TAR" | grep 'auditor.toml' | cut -c1)" == "-" ]] || fail "symlinked cards must be copied as regular files, not dangling host links"

reset_world
mkdir -p "$WORK/noauditor"
cp "$CATALOG/reviewer.toml" "$WORK/noauditor/"
if run_launcher NANNA_CATALOG_SRC="$WORK/noauditor" "$BASH" "$LAUNCHER" --install-catalog > /dev/null 2> "$WORK/err"; then
  fail "--install-catalog must refuse a source without auditor.toml"
else
  grep -q 'auditor.toml' "$WORK/err" || fail "the refusal must name auditor.toml"
fi
! grep -q 'volume' "$FAKE_PODMAN_LOG" || fail "a refused install must not touch the volume"
if run_launcher NANNA_CATALOG_SRC="$WORK/absent" "$BASH" "$LAUNCHER" --install-catalog > /dev/null 2>&1; then fail "--install-catalog must refuse a missing source"; fi

reset_world
touch "$FAKE_STATE/container"
if run_launcher "$BASH" "$LAUNCHER" --install-catalog > /dev/null 2> "$WORK/err"; then
  fail "--install-catalog must refuse while the daemon container exists"
else
  grep -q -- '--stop' "$WORK/err" || fail "the refusal must point at --stop"
fi
! grep -q 'volume rm' "$FAKE_PODMAN_LOG" || fail "the config volume must not be removed under a live daemon"

reset_world
mkdir -p "$WORK/home/.config/nanna/agents"
cp "$CATALOG/auditor.toml" "$WORK/home/.config/nanna/agents/"
env -i FAKE_STATE="$FAKE_STATE" FAKE_PODMAN_LOG="$FAKE_PODMAN_LOG" FAKE_IMPORT_TAR="$FAKE_IMPORT_TAR" PATH="$WORK/bin:/usr/bin:/bin" HOME="$WORK/home" \
  "$BASH" "$LAUNCHER" --install-catalog > /dev/null 2>&1 || fail "the default catalog source is ~/.config/nanna/agents"
tar -tf "$FAKE_IMPORT_TAR" | grep -q 'agents/auditor.toml' || fail "the default source must be imported"

[[ ! -e "$HERE/../../scripts/lumo-config" ]] || fail "the special lumo card must be gone"

reset_world
out="$(run_launcher "$BASH" "$LAUNCHER" --print)"
grep -q '^name: nanna-coder$' <<< "$out" || fail "--print must show the server name"
grep -q '^command: /.*lumo-mcp.sh$' <<< "$out" || fail "--print must show the absolute launcher path as command"
[[ ! -s "$FAKE_PODMAN_LOG" ]] || fail "--print must not call podman"

run_launcher "$BASH" "$LAUNCHER" --help > "$WORK/out" || fail "--help must succeed"
for flag in --start --stop --restart --status --install-catalog --print; do
  grep -q -- "$flag" "$WORK/out" || fail "--help must list $flag"
done

mkdir -p "$WORK/nopodman"
ln -s "$(command -v dirname)" "$WORK/nopodman/dirname"
ln -s "$(command -v basename)" "$WORK/nopodman/basename"
if env -i PATH="$WORK/nopodman" HOME="$WORK" "$BASH" "$LAUNCHER" < /dev/null > /dev/null 2> "$WORK/err"; then
  fail "must fail when podman is missing"
else
  [[ $? -eq 127 ]] || fail "missing podman must exit 127"
  grep -qi 'podman' "$WORK/err" || fail "missing-podman error must mention podman"
fi

if "$BASH" "$LAUNCHER" --bogus > /dev/null 2>&1; then fail "unknown flag must be rejected"; fi

if [[ $FAILED -eq 0 ]]; then echo "PASS: lumo-mcp launcher"; fi
exit $FAILED
