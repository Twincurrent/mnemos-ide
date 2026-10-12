#!/usr/bin/env bash
#
# Prepares, but never pushes, the public history of this repo.
#
# The private history (owner notes, client data in old fixtures, per-commit
# session trailers) must never be published. Instead the public repo gets its
# own history: Mnemos IDE is published as a product of its own that is based on
# Warp, not as a branch of Warp's history. The first export is ONE root commit
# holding the current tree, minus the paths listed in .oss-exclude; each later
# export (--public) is ONE snapshot commit on top of the public repo's main.
# The Warp commit Mnemos is based on is named in the commit message and NOTICE.
#
# Usage: script/oss/export-public.sh --author "Name <email>" [--public URL] [--base SHA] [--rev REV] [--out DIR]
#   --author        identity for the commit. Required: it is published, so
#                   it is never taken from the local git config.
#   --public URL    the public repository; its main becomes the parent of the new
#                   snapshot commit. Omit only for the very first export.
#   --base SHA      upstream Warp commit Mnemos is based on, named in the commit
#                   message. Default: script/oss/UPSTREAM_BASE.
#   --rev REV       commit of this repo to export (default HEAD). Only committed
#                   content is exported; uncommitted edits are ignored.
#   --out DIR       where to build the clone (default: a new mktemp dir). Must not exist
#                   or be empty.
#
# Result: a standalone git repo in DIR with one branch, public-main, and no
# remotes. The script runs script/oss/leak-check.sh --strict on it and fails if
# that fails. It never pushes and never adds a remote; it prints the commands
# the owner runs to publish.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC="$(cd "$SCRIPT_DIR/../.." && pwd)"

BASE=""
AUTHOR=""
REV="HEAD"
OUT=""
PUBLIC_URL=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --base) BASE="$2"; shift 2 ;;
        --author) AUTHOR="$2"; shift 2 ;;
        --rev) REV="$2"; shift 2 ;;
        --out) OUT="$2"; shift 2 ;;
        --public) PUBLIC_URL="$2"; shift 2 ;;
        --push|--publish)
            echo "export-public: refusing to push. Publishing is a manual step; see the commands printed at the end." >&2
            exit 2 ;;
        -h|--help) sed -n '2,28p' "${BASH_SOURCE[0]}"; exit 0 ;;
        *) echo "export-public: unknown argument: $1" >&2; exit 2 ;;
    esac
done

if [[ -z "$BASE" && -f "$SCRIPT_DIR/UPSTREAM_BASE" ]]; then
    BASE="$(grep -v -E '^[[:space:]]*(#|$)' "$SCRIPT_DIR/UPSTREAM_BASE" | head -n 1 | tr -d '[:space:]')"
fi
if [[ ! "$BASE" =~ ^[0-9a-f]{40}$ ]]; then
    echo "export-public: need a full 40-hex upstream base SHA (--base or script/oss/UPSTREAM_BASE)" >&2
    exit 2
fi

if [[ ! "$AUTHOR" =~ ^([^<>]+[^<>[:space:]])[[:space:]]*\<([^<>[:space:]]+@[^<>[:space:]]+)\>$ ]]; then
    echo "export-public: need --author \"Name <email>\" for the import commit" >&2
    exit 2
fi
NAME="${BASH_REMATCH[1]}"
EMAIL="${BASH_REMATCH[2]}"

REV_SHA="$(git -C "$SRC" rev-parse --verify "$REV^{commit}")"

if [[ -z "$OUT" ]]; then
    OUT="$(mktemp -d "${TMPDIR:-/tmp}/mnemos-public.XXXXXX")"
else
    mkdir -p "$OUT"
    if [[ -n "$(ls -A "$OUT")" ]]; then
        echo "export-public: $OUT is not empty" >&2
        exit 2
    fi
    OUT="$(cd "$OUT" && pwd)"
fi
echo "export-public: building in $OUT"

git init -q -b public-main "$OUT"
cd "$OUT"
# No hooks, no remotes, nothing inherited from the private repo's config.
git config core.hooksPath /dev/null

# 1. The parent: the public repo's main, or none for the first export.
PARENT=""
if [[ -n "$PUBLIC_URL" ]]; then
    git -c credential.helper= fetch -q --no-tags "$PUBLIC_URL" "+refs/heads/main:refs/public/main"
    PARENT="$(git rev-parse refs/public/main)"
fi

# 2. The current tree, from committed content only. git archive carries no
#    history and no objects beyond the tree itself.
git -C "$SRC" archive --format=tar "$REV_SHA" | tar -xf -

# 3. Drop excluded paths (.oss-exclude: one path or glob per line, # comments).
EXCLUDE_FILE="$OUT/.oss-exclude"
if [[ -f "$EXCLUDE_FILE" ]]; then
    while IFS= read -r pat || [[ -n "$pat" ]]; do
        pat="${pat%%#*}"
        pat="$(echo "$pat" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')"
        [[ -z "$pat" ]] && continue
        pat="${pat#/}"
        pat="${pat%/}"
        # Unquoted on purpose: globs expand against the extracted tree.
        # shellcheck disable=SC2086
        for p in $pat; do
            [[ -e "$p" || -L "$p" ]] && rm -rf -- "$p" && echo "export-public: excluded $p"
        done
    done < "$EXCLUDE_FILE"
fi

# 4. One import commit on top of the base. -f: add exactly the archived files,
#    including any that a .gitignore rule would otherwise skip.
git add -A -f .
TREE="$(git write-tree)"
if [[ -z "$PARENT" ]]; then
    MSG="Mnemos IDE

Mnemos IDE is based on the Warp terminal client (github.com/warpdotdev/warp,
commit ${BASE:0:12}), which Denver Technologies, Inc. publishes under the
AGPL-3.0, with its UI framework under MIT. See NOTICE."
    PARENT_ARGS=()
else
    if [[ "$(git rev-parse "$PARENT^{tree}")" == "$TREE" ]]; then
        echo "export-public: $PUBLIC_URL main already has this tree; nothing to export" >&2
        exit 1
    fi
    MSG="Update Mnemos IDE

Snapshot of the Mnemos IDE tree. Based on Warp commit ${BASE:0:12}; see NOTICE."
    PARENT_ARGS=(-p "$PARENT")
fi
COMMIT="$(GIT_AUTHOR_NAME="$NAME" GIT_AUTHOR_EMAIL="$EMAIL" \
    GIT_COMMITTER_NAME="$NAME" GIT_COMMITTER_EMAIL="$EMAIL" \
    git commit-tree "$TREE" ${PARENT_ARGS[@]+"${PARENT_ARGS[@]}"} -m "$MSG")"
git update-ref refs/heads/public-main "$COMMIT"
git reset -q
git update-ref -d refs/public/main 2>/dev/null || true
echo "export-public: public-main = $COMMIT${PARENT:+ (parent $PARENT)}"

# 5. Sanity: exactly one new commit, nothing else referenced.
if [[ -n "$PARENT" ]]; then
    count="$(git rev-list --count "$PARENT..public-main")"
else
    count="$(git rev-list --count public-main)"
fi
if [[ "$count" != 1 ]]; then
    echo "export-public: expected 1 new commit, found $count" >&2
    exit 1
fi
if [[ -n "$(git remote)" ]]; then
    echo "export-public: unexpected remote in the export clone" >&2
    exit 1
fi
refs="$(git for-each-ref --format='%(refname)')"
if [[ "$refs" != "refs/heads/public-main" ]]; then
    echo "export-public: unexpected refs in the export clone:" >&2
    echo "$refs" >&2
    exit 1
fi

# 6. Git LFS objects, if the tree has any LFS pointers (today it has none).
#    Copy the objects public-main references from this repo's LFS store; the
#    printed publish steps upload them with `git lfs push`. The hooks stay disabled (core.hooksPath above),
#    so a plain `git push` never uploads them by itself.
if ! command -v git-lfs > /dev/null 2>&1; then
    echo "export-public: git-lfs is not installed; it is needed to carry the LFS objects" >&2
    exit 1
fi
SRC_LFS="$(git -C "$SRC" lfs env 2>/dev/null | sed -n 's/^LocalMediaDir=//p' | head -n 1)"
OUT_LFS="$(git rev-parse --absolute-git-dir)/lfs/objects"
missing=0
lfs_count=0
while read -r oid _; do
    [[ "$oid" =~ ^[0-9a-f]{64}$ ]] || continue
    rel="${oid:0:2}/${oid:2:2}/$oid"
    if [[ -n "$SRC_LFS" && -f "$SRC_LFS/$rel" ]]; then
        mkdir -p "$OUT_LFS/${oid:0:2}/${oid:2:2}"
        cp "$SRC_LFS/$rel" "$OUT_LFS/$rel"
        lfs_count=$((lfs_count + 1))
    else
        echo "export-public: LFS object $oid is not in $SRC's LFS store" >&2
        missing=1
    fi
done < <(git lfs ls-files --all --long)
if [[ $missing == 1 ]]; then
    echo "export-public: run 'git lfs fetch --all' in $SRC, then export again" >&2
    exit 1
fi
if ! git lfs fsck --objects > /dev/null; then
    echo "export-public: git lfs fsck failed in the export clone" >&2
    exit 1
fi
echo "export-public: copied $lfs_count LFS object(s)"

# 7. Leak check on exactly what would be published.
echo "export-public: running leak-check --strict"
if ! "$SCRIPT_DIR/leak-check.sh" --strict --commits "${PARENT:+$PARENT..}public-main" "$OUT"; then
    echo "export-public: leak-check failed; fix the source tree and run again. Nothing was pushed." >&2
    exit 1
fi

cat <<EOF

export-public: ready. Nothing has been pushed.

Review it:
  cd $OUT
  git log --oneline -3 public-main
  git show --stat public-main | head -40

Publish it (owner only):
  cd $OUT
  git remote add public git@github.com:<org>/<repo>.git
  git lfs push --all public public-main
  git push public public-main:main
(The first export goes to an empty repository; later ones, made with --public,
fast-forward its main.)

Run the LFS push first: hooks are disabled in this clone on purpose, so the
git push does not upload LFS objects, and GitHub rejects a push whose LFS
pointers have no objects behind them.

Never push the private repo's branches to the public remote.
EOF
