#!/usr/bin/env bash
# check-docs-links.sh
#
# Validates internal Markdown links inside docs/ci/*.md.
#
# Scope (deliberate):
#   - Relative paths (e.g. ../TESTING.md, ./architecture.md, scripts/foo.sh)
#   - In-document anchors (e.g. #section-heading)
# Explicitly OUT of scope:
#   - External HTTP(S) URLs are NOT checked. Doing so in CI introduces
#     network flake, rate-limit failures, and a minor SSRF-shaped risk.
#     If you need external link validation, run it locally with a tool
#     like lychee, on your own time.
#
# Exits non-zero on the first broken link.
#
# Usage: bash scripts/check-docs-links.sh

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

shopt -s nullglob
docs=(docs/ci/*.md)
if [ ${#docs[@]} -eq 0 ]; then
    echo "error: no docs found under docs/ci/" >&2
    exit 1
fi

errors=0

# Extract markdown links of the form [text](target) — a simple regex is
# sufficient because we author these files and don't need to handle
# pathological cases like nested parentheses in URLs.
#
# For each link target:
#   - skip if it starts with http://, https://, or mailto:
#   - split "path#anchor" into path and anchor
#   - if path is non-empty, resolve relative to the file's directory and
#     check the file exists
#   - if anchor is non-empty, grep the target file for a matching heading
check_file() {
    local file="$1"
    local dir
    dir="$(dirname "$file")"

    # Grep all [text](target) pairs. Using Python would be cleaner, but a
    # POSIX-ish shell pipeline keeps this tool dependency-free.
    local line_num=0
    while IFS= read -r line; do
        line_num=$((line_num + 1))
        # Find every (target) on the line; iterate with a simple loop.
        local rest="$line"
        while [[ "$rest" =~ \[([^\]]+)\]\(([^\)]+)\) ]]; do
            local target="${BASH_REMATCH[2]}"
            rest="${rest#*"${BASH_REMATCH[0]}"}"

            # Skip external URLs and mailto:
            case "$target" in
                http://*|https://*|mailto:*|tel:*)
                    continue
                    ;;
            esac

            # Split into path and anchor.
            local path="${target%%#*}"
            local anchor=""
            if [[ "$target" == *"#"* ]]; then
                anchor="${target#*#}"
            fi

            # Pure anchor, same file: check within this file.
            if [ -z "$path" ]; then
                if [ -n "$anchor" ]; then
                    if ! check_anchor "$file" "$anchor"; then
                        echo "error: $file:$line_num: anchor '#$anchor' not found in $file" >&2
                        errors=$((errors + 1))
                    fi
                fi
                continue
            fi

            # Resolve path relative to the source file's directory.
            local resolved
            if [[ "$path" = /* ]]; then
                resolved="$repo_root$path"
            else
                resolved="$dir/$path"
            fi

            if [ ! -e "$resolved" ]; then
                echo "error: $file:$line_num: target not found: $target (resolved: $resolved)" >&2
                errors=$((errors + 1))
                continue
            fi

            # If there's an anchor and the target is a markdown file, verify it.
            if [ -n "$anchor" ] && [[ "$resolved" == *.md ]]; then
                if ! check_anchor "$resolved" "$anchor"; then
                    echo "error: $file:$line_num: anchor '#$anchor' not found in $resolved" >&2
                    errors=$((errors + 1))
                fi
            fi
        done
    done < "$file"
}

# Convert a markdown heading to the GitHub-style slug and compare.
# Simple algorithm: lowercase, drop non-alnum (keep hyphens), collapse spaces
# to hyphens. This matches GitHub's behavior closely enough for our docs.
slugify() {
    local s="$1"
    s="${s,,}"
    # Replace spaces with hyphens.
    s="${s// /-}"
    # Strip anything that is not a-z, 0-9, or hyphen.
    s="$(printf '%s' "$s" | LC_ALL=C sed 's/[^a-z0-9-]//g')"
    printf '%s' "$s"
}

check_anchor() {
    local file="$1"
    local anchor="$2"
    # Read each heading line, slugify it, compare.
    while IFS= read -r heading; do
        # Strip leading #'s and whitespace.
        heading="${heading#"${heading%%[!#]*}"}"
        heading="${heading# }"
        if [ "$(slugify "$heading")" = "$anchor" ]; then
            return 0
        fi
    done < <(grep -E '^#{1,6} ' "$file" || true)
    return 1
}

for doc in "${docs[@]}"; do
    check_file "$doc"
done

if [ "$errors" -ne 0 ]; then
    echo ""
    echo "check-docs-links.sh: $errors broken internal link(s) found" >&2
    exit 1
fi

echo "check-docs-links.sh: OK (${#docs[@]} file(s) scanned, internal links only)"
