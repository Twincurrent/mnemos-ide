#!/bin/bash
# SPDX-License-Identifier: MIT OR AGPL-3.0-only
# Checks the mnemos-agents Stop hook's background_tasks count against small
# transcript fixtures. Run: bash ide/claude-plugin/tests/background-tasks.sh
# [transcript.jsonl] (an optional real transcript is only reported, not
# checked). Needs jq. The hook runs from a temp copy whose mnemos_emit prints
# the payload, so nothing is written to a terminal.
set -u
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SCRIPTS="$HERE/../plugins/mnemos-agents/scripts"
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
cp "$SCRIPTS"/*.sh "$WORK/"
echo 'mnemos_emit() { printf "%s\n" "$1"; }' >> "$WORK/lib.sh"
export MNEMOS_TERMINAL=1 MNEMOS_CLI_AGENT_PROTOCOL_VERSION=1

count() {
    jq -nc --arg t "$1" '{session_id: "s", cwd: "/tmp/p", transcript_path: $t}' \
        | bash "$WORK/on-stop.sh" | jq -r '.background_tasks'
}

# Fixture lines.
BASH_BG='{"type":"user","toolUseResult":{"backgroundTaskId":"bash1","stdout":""}}'
MONITOR='{"type":"user","toolUseResult":{"taskId":"mon1","timeoutMs":900000,"persistent":false}}'
AGENT='{"type":"user","toolUseResult":{"isAsync":true,"status":"async_launched","agentId":"agent1"}}'
TEAMMATE='{"type":"user","toolUseResult":{"status":"teammate_spawned","name":"helper"}}'
AGENT2='{"type":"user","toolUseResult":{"isAsync":true,"status":"async_launched","agentId":"agent2"}}'
AGENT2_DONE='{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>agent2</task-id>\n<status>completed</status>\n</task-notification>"}'
MONITOR_EXPIRED='{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>mon1</task-id>\n<event>[Monitor expired after 15m with no events delivered.]</event>\n</task-notification>"}'
HELPER_IDLE='{"type":"user","message":{"role":"user","content":"Another Claude session sent a message:\n<teammate-message teammate_id=\"helper\" color=\"green\">\n{\"type\":\"idle_notification\",\"from\":\"helper\"}\n</teammate-message>"}}'
WAKE_HELPER='{"type":"assistant","message":{"content":[{"type":"tool_use","name":"SendMessage","input":{"to":"helper","message":"more"}}]}}'
START='{"type":"attachment","attachment":{"type":"hook_success","hookName":"SessionStart:startup","hookEvent":"SessionStart"}}'
RESUME='{"type":"attachment","attachment":{"type":"hook_success","hookName":"SessionStart:resume","hookEvent":"SessionStart"}}'
COMPACT='{"type":"attachment","attachment":{"type":"hook_success","hookName":"SessionStart:compact","hookEvent":"SessionStart"}}'
ORPHANS='{"type":"queue-operation","operation":"enqueue","content":"<task-notification>\n<task-id>__orphan_summary__:shell</task-id>\n<summary>2 background tasks were stopped</summary>\n</task-notification>"}'
MENTION='{"type":"user","message":{"role":"user","content":"talk about __orphan_summary__ and SessionStart:resume"}}'

FAIL=0
check() {
    local name="$1" want="$2"
    shift 2
    printf '%s\n' "$@" > "$WORK/t.jsonl"
    local got
    got=$(count "$WORK/t.jsonl")
    if [ "$got" = "$want" ]; then
        echo "ok   $name ($got)"
    else
        echo "FAIL $name: want $want, got $got"
        FAIL=1
    fi
}

check "four kinds started"                 4 "$START" "$BASH_BG" "$MONITOR" "$AGENT" "$TEAMMATE"
check "ends: completed, monitor expired"   2 "$START" "$BASH_BG" "$MONITOR" "$AGENT" "$AGENT2" "$AGENT2_DONE" "$MONITOR_EXPIRED"
check "teammate idle, then woken"          1 "$TEAMMATE" "$HELPER_IDLE" "$WAKE_HELPER"
check "teammate idle"                      0 "$TEAMMATE" "$WAKE_HELPER" "$HELPER_IDLE"
check "started, then resume -> 0"          0 "$START" "$BASH_BG" "$MONITOR" "$AGENT" "$TEAMMATE" "$RESUME"
check "started after resume still count"   2 "$START" "$BASH_BG" "$MONITOR" "$RESUME" "$AGENT" "$TEAMMATE"
check "orphan summary clears"              1 "$BASH_BG" "$AGENT" "$ORPHANS" "$AGENT2"
check "compact keeps tasks"                2 "$BASH_BG" "$AGENT" "$COMPACT"
check "mere mentions change nothing"       2 "$BASH_BG" "$AGENT" "$MENTION"
check "half-written last line is skipped"  1 "$BASH_BG" '{"type":"user","toolUse'

mode() {
    jq -nc --arg m "$1" '{session_id: "s", cwd: "/tmp/p", prompt: "hi", permission_mode: $m}' \
        | bash "$WORK/on-prompt-submit.sh" | jq -r '.permission_mode // "none"'
}
check_mode() {
    local got
    got=$(mode "$1")
    if [ "$got" = "$2" ]; then
        echo "ok   permission_mode $1 -> $got"
    else
        echo "FAIL permission_mode $1: want $2, got $got"
        FAIL=1
    fi
}
check_mode bypassPermissions bypassPermissions
check_mode auto auto
check_mode "" none
check_mode 'x"; rm -rf /' none

if [ $# -gt 0 ]; then
    echo "real transcript $1: background_tasks=$(count "$1")"
fi
exit $FAIL
