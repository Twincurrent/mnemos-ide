#!/bin/bash
# SPDX-License-Identifier: MIT OR AGPL-3.0-only
# Checks team.sh and the on-team-context.sh hook against a scratch repository
# with a second worktree. Run: bash ide/claude-plugin/tests/team-context.sh
# Needs git and jq.
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS="$HERE/../plugins/mnemos-agents/scripts"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
unset MNEMOS_TEAM_DIR

REPO="$WORK/repo"
git init -q "$REPO"
git -C "$REPO" -c user.name=t -c user.email=t@example.org commit -q --allow-empty -m init
git -C "$REPO" worktree add -q "$WORK/wt" -b wt

hook() { # <event> <session> <cwd>
    jq -nc --arg e "$1" --arg s "$2" --arg c "$3" '{hook_event_name: $e, session_id: $s, cwd: $c}' \
        | bash "$SCRIPTS/on-team-context.sh"
}
ctx() { jq -r '.hookSpecificOutput.additionalContext // empty'; }

FAIL=0
check() { # <name> <condition result 0/1>
    if [ "$2" = 0 ]; then echo "ok   $1"; else echo "FAIL $1"; FAIL=1; fi
}

out=$(hook UserPromptSubmit s1 "$REPO")
check "silent without team files" "$([ -z "$out" ]; echo $?)"

dir_main=$(cd "$REPO" && bash "$SCRIPTS/team.sh" dir)
dir_wt=$(cd "$WORK/wt" && bash "$SCRIPTS/team.sh" dir)
check "worktrees share one team dir" "$([ "$dir_main" = "$dir_wt" ]; echo $?)"

printf 'No pushes until everything is done.\n' > "$dir_main/rules.md"
out=$(hook UserPromptSubmit s1 "$WORK/wt" | ctx)
check "new rules are shown" "$(grep -q 'No pushes' <<< "$out"; echo $?)"
out=$(hook UserPromptSubmit s1 "$WORK/wt")
check "unchanged rules are not shown again" "$([ -z "$out" ]; echo $?)"
out=$(hook UserPromptSubmit s2 "$REPO" | ctx)
check "another session sees them once" "$(grep -q 'No pushes' <<< "$out"; echo $?)"

printf 'Pushing is allowed now.\n' > "$dir_main/rules.md"
out=$(hook UserPromptSubmit s1 "$WORK/wt" | ctx)
check "changed rules are shown again" "$(grep -q 'Pushing is allowed' <<< "$out"; echo $?)"

(cd "$WORK/wt" && bash "$SCRIPTS/team.sh" set tester working "visual QA") || FAIL=1
(cd "$REPO" && bash "$SCRIPTS/team.sh" set builder review "OAuth" abc1234) || FAIL=1
(cd "$REPO" && bash "$SCRIPTS/team.sh" set tester done) || FAIL=1
board=$(cd "$REPO" && bash "$SCRIPTS/team.sh" board)
check "set merges into an existing row" \
    "$([ "$(jq -r '.tester.state + "/" + .tester.task' <<< "$board")" = "done/visual QA" ]; echo $?)"
check "sha is recorded" "$([ "$(jq -r '.builder.sha' <<< "$board")" = abc1234 ]; echo $?)"

out=$(hook UserPromptSubmit s1 "$WORK/wt" | ctx)
check "board change is shown" "$(grep -q 'builder: review | OAuth | sha abc1234' <<< "$out"; echo $?)"
check "unchanged rules stay out of a board update" "$(! grep -q 'Team rules' <<< "$out"; echo $?)"

out=$(hook SessionStart s1 "$WORK/wt" | ctx)
check "SessionStart shows everything again" \
    "$(grep -q 'Pushing is allowed' <<< "$out" && grep -q 'tester: done' <<< "$out"; echo $?)"
check "SessionStart names its event" \
    "$([ "$(hook SessionStart s3 "$REPO" | jq -r .hookSpecificOutput.hookEventName)" = SessionStart ]; echo $?)"

(cd "$REPO" && bash "$SCRIPTS/team.sh" remove tester) || FAIL=1
check "remove drops a row" "$([ "$(cd "$REPO" && bash "$SCRIPTS/team.sh" board | jq -r 'has("tester")')" = false ]; echo $?)"

out=$(hook UserPromptSubmit '../evil' "$REPO")
check "a session id with a path is ignored" "$([ -z "$out" ] && [ ! -e "$dir_main/evil" ]; echo $?)"

VER=$(jq -r .version "$SCRIPTS/../.claude-plugin/plugin.json")
FAKEHOME="$WORK/home"
mkdir -p "$FAKEHOME/.claude/plugins/cache/mnemos/mnemos-agents/$VER/scripts"
touch "$FAKEHOME/.claude/plugins/cache/mnemos/mnemos-agents/$VER/scripts/team.sh"
(cd "$REPO" && bash "$SCRIPTS/team.sh" set hinter idle) || FAIL=1
out=$(HOME="$FAKEHOME" hook UserPromptSubmit s1 "$REPO" | ctx)
check "board hint names the installed team.sh" \
    "$(grep -q "$FAKEHOME/.claude/plugins/cache/mnemos/mnemos-agents/$VER/scripts/team.sh" <<< "$out"; echo $?)"

out=$(hook UserPromptSubmit s9 "$WORK")
check "silent outside a git repository" "$([ -z "$out" ]; echo $?)"

exit $FAIL
