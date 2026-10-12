#!/usr/bin/env bash
#
# Checks that every commit in a range carries a Developer Certificate of Origin
# sign-off (CONTRIBUTING.md, "Sign off your commits"): a
#   Signed-off-by: Name <email>
# trailer whose email matches the commit's author email (case-insensitive).
# Merge commits are skipped. `git commit -s` adds the trailer.
#
# Usage: script/oss/check-dco.sh BASE HEAD
#   Checks the commits in BASE..HEAD. CI (.github/workflows/dco.yml) passes the
#   pull request's base and head commits.

set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 BASE HEAD" >&2
    exit 2
fi
BASE="$1"
HEAD="$2"

status=0
count=0
while IFS= read -r sha; do
    [[ -z "$sha" ]] && continue
    count=$((count + 1))
    email="$(git log -1 --format='%ae' "$sha")"
    subject="$(git log -1 --format='%s' "$sha")"
    if git log -1 --format='%(trailers:key=Signed-off-by,valueonly)' "$sha" \
        | grep -q -i -F "<$email>"; then
        echo "dco: ok      ${sha:0:12} $subject"
    else
        echo "dco: MISSING ${sha:0:12} $subject (no Signed-off-by for <$email>)" >&2
        status=1
    fi
done < <(git rev-list --no-merges "$BASE..$HEAD")

if [[ $status -ne 0 ]]; then
    echo "dco: sign off each commit with 'git commit -s' (for existing commits:" >&2
    echo "dco:   git rebase --signoff $BASE), then push again. See CONTRIBUTING.md." >&2
else
    echo "dco: $count commit(s) signed off"
fi
exit $status
