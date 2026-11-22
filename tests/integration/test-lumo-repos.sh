#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPT="$HERE/../../scripts/lumo-repos.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
FAILED=0

fail() { echo "FAIL: $1"; FAILED=1; }

mkdir -p "$WORK/bin" "$WORK/volumes"
cat > "$WORK/bin/podman" <<'FAKE'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$FAKE_PODMAN_LOG"
case "${1:-} ${2:-}" in
  "volume exists") [[ -e "$FAKE_VOLUME_DIR/$3" ]]; exit $? ;;
  "volume create") touch "$FAKE_VOLUME_DIR/$3"; exit 0 ;;
  "volume rm") rm -f "$FAKE_VOLUME_DIR/${@: -1}"; exit 0 ;;
esac
if [[ -n "${FAKE_FAIL_MATCH:-}" && "$*" == *"$FAKE_FAIL_MATCH"* ]]; then
  echo "fatal: could not read Username" >&2
  exit 128
fi
exit 0
FAKE
chmod +x "$WORK/bin/podman"
export FAKE_PODMAN_LOG="$WORK/log" FAKE_VOLUME_DIR="$WORK/volumes"
CATALOG="$WORK/catalog"
mkdir -p "$CATALOG/prompts"
cat > "$CATALOG/auditor.toml" <<'CARD'
[identity]
name = "auditor"

[scope]
repos = ["github.com/DominicBurkart/marigold", "github.com/DominicBurkart/personal-site-actix", "github.com/DominicBurkart/nanna-coder"]
CARD
cat > "$CATALOG/one-track-api.toml" <<'CARD'
[scope]
repos = ["github.com/1-Track/one_track", "github.com/DominicBurkart/marigold"]
CARD
printf '[identity]\nname = "no-repos"\n' > "$CATALOG/no-repos.toml"
printf 'repos = ["github.com/evil/ignored"]\n' > "$CATALOG/prompts/note.md"
: > "$FAKE_PODMAN_LOG"

run_repos() {
  env -i FAKE_PODMAN_LOG="$FAKE_PODMAN_LOG" FAKE_VOLUME_DIR="$FAKE_VOLUME_DIR" FAKE_FAIL_MATCH="${FAKE_FAIL_MATCH:-}" NANNA_CATALOG_SRC="$CATALOG" \
    PATH="$WORK/bin:/usr/bin:/bin" HOME="$WORK/home" "$@"
}

listing="$(run_repos "$BASH" "$SCRIPT" --list)"
expected=$'marigold\tnanna-repo-marigold\tgithub.com/DominicBurkart/marigold\npersonal-site-actix\tnanna-repo-personal-site-actix\tgithub.com/DominicBurkart/personal-site-actix\nnanna-coder\tnanna-repo-nanna-coder\tgithub.com/DominicBurkart/nanna-coder\none_track\tnanna-repo-one_track\tgithub.com/1-Track/one_track'
[[ "$listing" == "$expected" ]] || fail "--list must show the de-duplicated union of the catalog's repos, got: $listing"
[[ ! -s "$FAKE_PODMAN_LOG" ]] || fail "--list must not call podman"

: > "$FAKE_PODMAN_LOG"
run_repos "$BASH" "$SCRIPT" > "$WORK/out" 2>&1 || fail "initial population must succeed: $(cat "$WORK/out")"
for r in marigold personal-site-actix nanna-coder one_track; do
  [[ -e "$FAKE_VOLUME_DIR/nanna-repo-$r" ]] || fail "volume for $r must be created"
done
[[ "$(grep -c '^volume create ' "$FAKE_PODMAN_LOG")" -eq 4 ]] || fail "exactly four volumes must be created"
[[ "$(grep -c 'clone' "$FAKE_PODMAN_LOG")" -eq 4 ]] || fail "each new volume must be populated with a clone"
! grep -q 'fetch' "$FAKE_PODMAN_LOG" || fail "a new volume must not be fetched"
grep -q 'clone .*https://github.com/DominicBurkart/marigold /repo' "$FAKE_PODMAN_LOG" || fail "clone must use the anonymous https slug URL"
grep -q 'nanna-repo-marigold:/repo' "$FAKE_PODMAN_LOG" || fail "the populate step must mount the repo volume"
for v in $(grep -o -- '-v [^ ]*' "$FAKE_PODMAN_LOG" | cut -d' ' -f2); do
  [[ "$v" =~ ^nanna-repo-[A-Za-z0-9_.-]+:/repo$ ]] || fail "mount must be a named volume at /repo, got: $v"
done

: > "$FAKE_PODMAN_LOG"
run_repos "$BASH" "$SCRIPT" > /dev/null 2>&1 || fail "refresh must succeed"
! grep -q '^volume create' "$FAKE_PODMAN_LOG" || fail "an existing volume must not be re-created"
! grep -q 'clone' "$FAKE_PODMAN_LOG" || fail "an existing volume must be fetched, not cloned"
[[ "$(grep -c 'fetch --prune' "$FAKE_PODMAN_LOG")" -eq 4 ]] || fail "each existing volume must be refreshed with fetch --prune"

: > "$FAKE_PODMAN_LOG"
run_repos "$BASH" "$SCRIPT" --only marigold > /dev/null 2>&1 || fail "--only must succeed"
[[ "$(grep -c 'fetch --prune' "$FAKE_PODMAN_LOG")" -eq 1 ]] || fail "--only must touch exactly one repo"
grep -q 'nanna-repo-marigold' "$FAKE_PODMAN_LOG" || fail "--only marigold must touch the marigold volume"
! grep -q 'nanna-repo-one_track' "$FAKE_PODMAN_LOG" || fail "--only must not touch other volumes"
if run_repos "$BASH" "$SCRIPT" --only bogus > /dev/null 2>&1; then fail "--only with a repo outside the catalog must be rejected"; fi

! grep -q 'security-opt\|label=\|:z\|:Z\|privileged' "$FAKE_PODMAN_LOG" || fail "must never alter SELinux labelling or privileges"
! grep -qi 'token\|password\|ghp_\|@github.com' "$FAKE_PODMAN_LOG" || fail "credentials must never appear in argv"
! grep -q "$WORK/home\|-v /" "$FAKE_PODMAN_LOG" || fail "must not mount any host path"

rm -f "$FAKE_VOLUME_DIR/nanna-repo-one_track"
: > "$FAKE_PODMAN_LOG"
if FAKE_FAIL_MATCH="1-Track/one_track" run_repos "$BASH" "$SCRIPT" > "$WORK/out" 2> "$WORK/err"; then
  fail "a repo that cannot be cloned anonymously must make the script exit non-zero"
else
  grep -qi 'credentials are not supported' "$WORK/err" || fail "private-repo failure must say credentials are not supported"
fi
[[ ! -e "$FAKE_VOLUME_DIR/nanna-repo-one_track" ]] || fail "a volume whose clone failed must be removed"
[[ -e "$FAKE_VOLUME_DIR/nanna-repo-marigold" ]] || fail "other repos must be unaffected by one failure"

mkdir -p "$WORK/nopodman"
ln -s "$(command -v dirname)" "$WORK/nopodman/dirname"
ln -s "$(command -v basename)" "$WORK/nopodman/basename"
if env -i PATH="$WORK/nopodman" HOME="$WORK" "$BASH" "$SCRIPT" > /dev/null 2> "$WORK/err"; then
  fail "must fail when podman is missing"
else
  grep -qi 'podman' "$WORK/err" || fail "missing-podman error must mention podman"
fi
: > "$FAKE_PODMAN_LOG"
if run_repos NANNA_CATALOG_SRC="$WORK/none" "$BASH" "$SCRIPT" > /dev/null 2>&1; then fail "a missing catalog must fail"; fi
[[ ! -s "$FAKE_PODMAN_LOG" ]] || fail "a missing catalog must not reach podman"
mkdir -p "$WORK/bare"
printf '[identity]\nname = "x"\n' > "$WORK/bare/x.toml"
if run_repos NANNA_CATALOG_SRC="$WORK/bare" "$BASH" "$SCRIPT" --list > /dev/null 2>&1; then fail "a catalog naming no repository must fail"; fi
mkdir -p "$WORK/home/.config/nanna/agents" "$WORK/xdg/nanna/agents"
printf 'repos = ["github.com/o/home-repo"]\n' > "$WORK/home/.config/nanna/agents/a.toml"
printf 'repos = ["github.com/o/xdg-repo"]\n' > "$WORK/xdg/nanna/agents/a.toml"
env -i PATH="/usr/bin:/bin" HOME="$WORK/home" "$BASH" "$SCRIPT" --list | grep -q '^home-repo' || fail "the default catalog is ~/.config/nanna/agents"
env -i PATH="/usr/bin:/bin" HOME="$WORK/home" XDG_CONFIG_HOME="$WORK/xdg" "$BASH" "$SCRIPT" --list | grep -q '^xdg-repo' || fail "XDG_CONFIG_HOME must override the home default"
if "$BASH" "$SCRIPT" --bogus > /dev/null 2>&1; then fail "unknown flag must be rejected"; fi

if [[ $FAILED -eq 0 ]]; then echo "PASS: lumo-repos"; fi
exit $FAILED
