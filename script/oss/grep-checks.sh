#!/usr/bin/env bash
#
# The cheap text checks that guard the public tree, in one entry point for
# script/presubmit, script/ide-check and CI (.github/workflows/pr.yml). Needs
# only git and bash, no build.
#
#   1. script/check-namespaces: install sources, domains and artwork Mnemos does
#      not own (Warp's renamed org, npm scope and domain, Warp's own domain, Warp's logo
#      glyph and company name, the private repository).
#   2. Billing and upsell copy from upstream Warp ("Build plan", "Mnemos
#      credits", "Pro plan") under app/ and crates/: Mnemos sells nothing.
#
# Usage: script/oss/grep-checks.sh [--strict] [DIR]
#   DIR       git work tree to scan (default: this repo; only tracked files).
#   --strict  ignore "temp:" allowlist entries (see below).
#
# Intentional billing-phrase mentions go in script/oss/grep-checks-allowlist.txt,
# one "<path> <regex>" per line: a hit is ignored when its path equals <path>
# and the matched line matches <regex> (extended, case-insensitive). Prefix the
# path with "temp:" for a hit another change is removing: honoured in a normal
# run, ignored under --strict. Keep every entry commented.
#
# Exits non-zero when any check fails; every check always runs.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

STRICT=0
TARGET=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --strict) STRICT=1; shift ;;
        -h|--help) sed -n '2,23p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) TARGET="$1"; shift ;;
    esac
done
TARGET="$(cd "${TARGET:-$REPO_ROOT}" && pwd)"
ALLOWLIST="$SCRIPT_DIR/grep-checks-allowlist.txt"

status=0

"$REPO_ROOT/script/check-namespaces" "$TARGET" || status=1

# Split so this file does not match itself.
BILLING_PATTERN="build ""plan|mnemos ""credits|pro ""plan"

allow_paths=()
allow_res=()
if [[ -f "$ALLOWLIST" ]]; then
    while IFS= read -r entry || [[ -n "$entry" ]]; do
        [[ "$entry" =~ ^[[:space:]]*(#|$) ]] && continue
        if [[ "$entry" == temp:* ]]; then
            entry="${entry#temp:}"
            [[ "$STRICT" == 1 ]] && continue
        fi
        allow_paths+=("${entry%% *}")
        allow_res+=("${entry#* }")
    done < "$ALLOWLIST"
fi

billing_hits="$(git -C "$TARGET" grep -n -I -i -E "$BILLING_PATTERN" -- app crates || true)"
billing_status=0
while IFS= read -r hit; do
    [[ -z "$hit" ]] && continue
    path="${hit%%:*}"
    rest="${hit#*:}"
    text="${rest#*:}"
    allowed=0
    for i in "${!allow_paths[@]}"; do
        if [[ "$path" == "${allow_paths[$i]}" ]] && grep -q -i -E -e "${allow_res[$i]}" <<< "$text"; then
            allowed=1
            break
        fi
    done
    if [[ $allowed == 0 ]]; then
        echo "grep-checks: billing copy: ${hit:0:300}" >&2
        billing_status=1
    fi
done <<< "$billing_hits"

if [[ $billing_status -ne 0 ]]; then
    echo "grep-checks: remove the billing copy above, or allowlist an intentional mention in $ALLOWLIST" >&2
    status=1
else
    echo "grep-checks: billing copy ok"
fi

exit $status
