#!/usr/bin/env bash
#
# Fails if the tree contains private data that must never reach the public repo:
# the owners' home paths and user names, client and private-repo names, admin
# account UUIDs, Warp employee addresses, Sentry DSNs and, in the sqlite fixtures,
# Warp-internal hosts, repos and tokens. Then runs gitleaks over
# the same tree when it is installed.
#
# Usage: script/oss/leak-check.sh [--strict] [--commits RANGE] [DIR]
#   DIR       git work tree to scan (default: this repo). Only tracked files are
#             scanned, binary files included (the sqlite fixtures).
#   --strict  also fail when the private pattern file or gitleaks is missing.
#             script/oss/export-public.sh runs it this way.
#   --commits RANGE  also scan author, committer and message of the commits in
#             RANGE (for example BASE..public-main), since they get published too.
#
# Patterns come from two places:
#   - PUBLIC_PATTERNS below: safe to publish, checked everywhere.
#   - A private pattern file, one extended regex per line (# comments allowed),
#     kept outside the repo because the list itself names clients and accounts.
#     Path: $MNEMOS_LEAK_PATTERNS, else ../mnemos-private/oss/leak-patterns.txt
#     next to the repo. Contributors do not have it and do not need it.
#
# Intentional mentions are allowlisted in script/oss/leak-allowlist.txt, one
# "<path> <regex>" per line: a hit is ignored when its path matches <path>
# exactly and the matched line matches <regex>. Keep every entry commented.
# Entries prefixed "temp:" mark a leak another change is removing; --strict
# ignores them.
#
# Matching is case-insensitive.
#
# Compressed files would hide their contents from both scanners, so the check
# reads every tracked file's magic bytes. Gzip files (the alacritty ref-test
# recordings) are inflated and scanned like any other file, gitleaks included;
# their hits are reported, and allowlisted, under the .gz path. Any other
# compressed or archive format (zip, xz, bzip2, zstd, 7z) fails the check unless
# its path is in COMPRESSED_ALLOWLIST below, with a comment saying why its
# contents are safe.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

STRICT=0
TARGET=""
COMMITS=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --strict) STRICT=1; shift ;;
        --commits) COMMITS="$2"; shift 2 ;;
        -h|--help) awk 'NR > 1 && /^#/ { print; next } NR > 1 { exit }' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) TARGET="$1"; shift ;;
    esac
done
TARGET="$(cd "${TARGET:-$REPO_ROOT}" && pwd)"

PUBLIC_PATTERNS=(
    '/Users/hans([^A-Za-z0-9]|$)'
    '-Users-hans(-|$)'
    'C:\\{1,2}Users\\{1,2}hans'
    'C:/Users/hans'
    '/home/hans([^A-Za-z0-9]|$)'
    'hansp'
    '@warp\.dev'
    '[0-9a-f]{32}@o[0-9]+\.ingest(\.[a-z]+)?\.sentry\.io'
)

# Checked only in the binary sqlite fixtures (*.sqlite). They were captured from Warp
# employees' databases, so Warp-internal hosts, repos and tokens can hide in cloud
# object rows; in source these names are legitimate upstream references.
FIXTURE_PATTERNS=(
    'warpdotdev'
    'staging\.warp\.dev'
    'warp-internal'
    'warp_eng'
    'warp-terminal-dev'
    'sentry\.io/api'
)

# Tracked compressed files, other than gzip, that the check may not read. Path,
# exactly as git ls-files prints it. Empty today: the only compressed files are
# gzip, which are inflated and scanned.
COMPRESSED_ALLOWLIST=(
)

PRIVATE_FILE="${MNEMOS_LEAK_PATTERNS:-$(cd "$REPO_ROOT/.." && pwd)/mnemos-private/oss/leak-patterns.txt}"
ALLOWLIST="$TARGET/script/oss/leak-allowlist.txt"
[[ -f "$ALLOWLIST" ]] || ALLOWLIST="$REPO_ROOT/script/oss/leak-allowlist.txt"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

printf '%s\n' "${PUBLIC_PATTERNS[@]}" > "$WORK/patterns"
printf '%s\n' "${FIXTURE_PATTERNS[@]}" > "$WORK/fixture-patterns"
if [[ -f "$PRIVATE_FILE" ]]; then
    grep -v -E '^[[:space:]]*(#|$)' "$PRIVATE_FILE" >> "$WORK/patterns" || true
    echo "leak-check: private patterns from $PRIVATE_FILE"
elif [[ "$STRICT" == 1 ]]; then
    echo "leak-check: private pattern file not found: $PRIVATE_FILE" >&2
    echo "leak-check: --strict needs it (set MNEMOS_LEAK_PATTERNS)" >&2
    exit 2
else
    echo "leak-check: no private pattern file, checking public patterns only"
fi

# The pattern list and the allowlist would match themselves.
EXCLUDES=(':!script/oss/leak-check.sh' ':!script/oss/leak-allowlist.txt')

set +e
git -C "$TARGET" grep --text -n -i -E -f "$WORK/patterns" -- . "${EXCLUDES[@]}" > "$WORK/hits"
status=$?
set -e
if [[ $status -gt 1 ]]; then
    echo "leak-check: git grep failed in $TARGET" >&2
    exit 2
fi
set +e
git -C "$TARGET" grep --text -n -i -E -f "$WORK/fixture-patterns" -- '*.sqlite' >> "$WORK/hits"
status=$?
set -e
if [[ $status -gt 1 ]]; then
    echo "leak-check: git grep over the sqlite fixtures failed in $TARGET" >&2
    exit 2
fi

# Compressed files, found by their magic bytes rather than their names. One perl
# process for the whole tree; symlinks are skipped (git grep does not follow them
# either).
: > "$WORK/archive-hits"
mkdir -p "$WORK/inflated"
git -C "$TARGET" ls-files -z | (cd "$TARGET" && perl -0 -ne '
    chomp;
    next if -l $_ || ! -f $_;
    open(my $fh, "<:raw", $_) or next;
    my $b = "";
    read($fh, $b, 6);
    close $fh;
    my $kind =
        substr($b, 0, 2) eq "\x1f\x8b"                      ? "gzip"
      : substr($b, 0, 4) eq "PK\x03\x04"                    ? "zip"
      : $b eq "\xfd7zXZ\x00"                               ? "xz"
      : $b =~ /\ABZh[1-9]/                                  ? "bzip2"
      : substr($b, 0, 4) eq "\x28\xb5\x2f\xfd"             ? "zstd"
      : $b eq "7z\xbc\xaf\x27\x1c"                         ? "7z"
      : "";
    print "$kind\t$_\n" if $kind;
') > "$WORK/compressed"
inflated=0
while IFS=$'\t' read -r kind path; do
    [[ -z "$path" ]] && continue
    if [[ "$kind" == gzip ]]; then
        mkdir -p "$WORK/inflated/$(dirname "$path")"
        # Capped, so a decompression bomb cannot fill the disk; 256 MiB is far
        # above any fixture.
        if ! gzip -dc < "$TARGET/$path" 2>/dev/null | head -c 268435456 > "$WORK/inflated/$path"; then
            echo "$path:0:gzip file the leak check cannot inflate" >> "$WORK/archive-hits"
            continue
        fi
        inflated=$((inflated + 1))
        continue
    fi
    allowed=0
    for entry in "${COMPRESSED_ALLOWLIST[@]+"${COMPRESSED_ALLOWLIST[@]}"}"; do
        [[ "$entry" == "$path" ]] && allowed=1
    done
    if [[ $allowed == 0 ]]; then
        echo "$path:0:$kind file the leak check cannot read; store it uncompressed or gzip, or list it in COMPRESSED_ALLOWLIST" >> "$WORK/archive-hits"
    fi
done < "$WORK/compressed"
if [[ $inflated -gt 0 ]]; then
    set +e
    (cd "$WORK/inflated" && grep -r -a -n -i -E -f "$WORK/patterns" .) | sed 's#^\./##' >> "$WORK/hits"
    status=${PIPESTATUS[0]}
    set -e
    if [[ $status -gt 1 ]]; then
        echo "leak-check: grep over the inflated gzip files failed" >&2
        exit 2
    fi
    echo "leak-check: inflated and scanned $inflated gzip file(s)"
fi

# Commit metadata is published with the tree. Reported as "commit <sha>:<n>:<text>",
# never allowlisted.
if [[ -n "$COMMITS" ]]; then
    git -C "$TARGET" log --format='@@%H%n%an <%ae>%n%cn <%ce>%n%B' "$COMMITS" > "$WORK/commits"
    awk '/^@@/ {sha = substr($0, 3); n = 0; next} {n++; print "commit " sha ":" n ":" $0}' "$WORK/commits" \
        | grep -i -E -f "$WORK/patterns" >> "$WORK/commit-hits" || true
fi

# Drop allowlisted hits. A hit line is "<path>:<line>:<text>".
: > "$WORK/left"
if [[ -s "$WORK/hits" ]]; then
    allow_paths=()
    allow_res=()
    if [[ -f "$ALLOWLIST" ]]; then
        while IFS= read -r entry || [[ -n "$entry" ]]; do
            [[ "$entry" =~ ^[[:space:]]*(#|$) ]] && continue
            if [[ "$entry" == temp:* ]]; then
                entry="${entry#temp:}"
                if [[ "$STRICT" == 1 ]]; then
                    echo "leak-check: --strict ignores temporary allowlist entry for ${entry%% *}" >&2
                    continue
                fi
            fi
            allow_paths+=("${entry%% *}")
            allow_res+=("${entry#* }")
        done < "$ALLOWLIST"
    fi
    while IFS= read -r hit; do
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
            # Binary lines can be huge; keep the report readable.
            printf '%s\n' "${hit:0:300}" >> "$WORK/left"
        fi
    done < "$WORK/hits"
fi

[[ -s "$WORK/commit-hits" ]] && cat "$WORK/commit-hits" >> "$WORK/left"
# Unreadable archives are never allowlisted here; COMPRESSED_ALLOWLIST is the only way.
[[ -s "$WORK/archive-hits" ]] && cat "$WORK/archive-hits" >> "$WORK/left"

failed=0
if [[ -s "$WORK/left" ]]; then
    count="$(wc -l < "$WORK/left" | tr -d ' ')"
    echo "leak-check: $count line(s) with private data:" >&2
    cat "$WORK/left" >&2
    failed=1
else
    echo "leak-check: pattern scan clean"
fi

if command -v gitleaks > /dev/null 2>&1; then
    # Scan only tracked files: copy them into a scratch dir (untracked build
    # output and local .env files are not part of what gets published).
    mkdir -p "$WORK/tree"
    # One tar process for the whole list: xargs would split a long list into
    # several archives, and the extracting tar stops after the first one.
    git -C "$TARGET" ls-files -z | (cd "$TARGET" && tar --null -T - -cf -) | (cd "$WORK/tree" && tar -xf -)
    tracked="$(git -C "$TARGET" ls-files | wc -l | tr -d ' ')"
    copied="$(find "$WORK/tree" \( -type f -o -type l \) | wc -l | tr -d ' ')"
    if [[ "$tracked" != "$copied" ]]; then
        echo "leak-check: copied $copied of $tracked tracked files for gitleaks" >&2
        exit 2
    fi
    gl_args=(dir "$WORK/tree" --no-banner --redact --verbose --exit-code 1)
    if [[ -f "$TARGET/.gitleaks.toml" ]]; then
        gl_args+=(--config "$TARGET/.gitleaks.toml")
    fi
    if gitleaks "${gl_args[@]}" > "$WORK/gitleaks.log" 2>&1; then
        echo "leak-check: gitleaks clean"
    else
        echo "leak-check: gitleaks reported findings:" >&2
        cat "$WORK/gitleaks.log" >&2
        failed=1
    fi
    if [[ $inflated -gt 0 ]]; then
        gl_args[1]="$WORK/inflated"
        if gitleaks "${gl_args[@]}" > "$WORK/gitleaks-inflated.log" 2>&1; then
            echo "leak-check: gitleaks clean over the inflated gzip files"
        else
            echo "leak-check: gitleaks reported findings in the inflated gzip files (paths are the .gz files):" >&2
            cat "$WORK/gitleaks-inflated.log" >&2
            failed=1
        fi
    fi
elif [[ "$STRICT" == 1 ]]; then
    echo "leak-check: gitleaks not installed and --strict was given" >&2
    exit 2
else
    echo "leak-check: gitleaks not installed, skipped"
fi

exit $failed
