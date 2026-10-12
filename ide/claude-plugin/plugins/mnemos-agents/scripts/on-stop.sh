#!/bin/bash
# SPDX-License-Identifier: MIT OR AGPL-3.0-only
# Stop -> emit "stop" with the last user prompt + assistant response, the
# Remote Control URL and how many background tasks are still running.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

mnemos_should_emit || exit 0
command -v jq >/dev/null 2>&1 || exit 0

INPUT=$(cat)

# Avoid double-firing when a Stop hook is already active.
[ "$(printf '%s' "$INPUT" | jq -r '.stop_hook_active // false' 2>/dev/null)" = "true" ] && exit 0

TRANSCRIPT_PATH=$(printf '%s' "$INPUT" | jq -r '.transcript_path // empty' 2>/dev/null)
# Small delay: the Stop hook fires before the turn is fully flushed to the transcript.
sleep 0.3

QUERY=""
RESPONSE=""
REMOTE_URL=""
BACKGROUND_TASKS=0
if [ -n "$TRANSCRIPT_PATH" ] && [ -f "$TRANSCRIPT_PATH" ]; then
    # One pass over the transcript. Lines that do not parse (a half-written
    # last line) are skipped.
    #
    # query: the last human prompt, i.e. "user" messages whose content is a
    #   plain string or has at least one {type:"text"} block (tool results
    #   have neither).
    # response: the last assistant text.
    # background_tasks: background work started and not yet reported finished,
    #   so Mnemos keeps the session "working" while Claude Code waits on it.
    #   The transcript is folded in order:
    #   - started: a tool result with `backgroundTaskId` (background Bash),
    #     `taskId` + `timeoutMs` (Monitor), `isAsync` + "async_launched" (a
    #     background Agent, id in `agentId`), or "teammate_spawned" (a named
    #     teammate, by name);
    #   - finished: a <task-notification> from the harness (a queue operation,
    #     a queued_command attachment or a task-notification user turn) with a
    #     terminal <status> or "Monitor expired", which ends every <task-id> it
    #     lists (an orphan summary lists several); for a teammate, its
    #     idle_notification <teammate-message>;
    #   - woken again: a SendMessage to a teammate that was spawned here;
    #   - all gone: Claude Code (re)started, so everything it ran before died
    #     with the old process. Markers: the SessionStart hook's attachment
    #     for a startup or resume (not compact or clear, which keep the
    #     process), and the orphan summary Claude Code sends after a restart.
    #     Without this a task killed with the process would count forever.
    SUMMARY=$(jq -nRc '
        def text_of:
            if type == "string" then .
            elif type == "array" then [.[] | select(.type? == "text") | .text] | join("\n")
            else "" end;
        def notification_text:
            if .type == "queue-operation" then .content | text_of
            elif .type == "attachment" and .attachment.type? == "queued_command" then .attachment.prompt | text_of
            elif .type == "user" and .origin.kind? == "task-notification" then .message.content | text_of
            else empty end;
        def started_id:
            select(.type == "user") | .toolUseResult | objects
            | if (.backgroundTaskId | type) == "string" then .backgroundTaskId
              elif (.taskId | type) == "string" and .timeoutMs != null then .taskId
              elif .isAsync == true and .status == "async_launched" and (.agentId | type) == "string" then .agentId
              else empty end;
        def ended_ids:
            notification_text | split("<task-notification>")[1:][]
            | select(test("<status>(completed|failed|stopped|killed)</status>") or contains("Monitor expired"))
            | scan("<task-id>([^<]+)</task-id>") | .[0];
        def teammate_spawned:
            select(.type == "user") | .toolUseResult | objects
            | select(.status == "teammate_spawned") | .name | strings | "teammate:" + .;
        def teammate_idle:
            if .type == "user" then .message.content | text_of
            elif .type == "attachment" and .attachment.type? == "queued_command" then .attachment.prompt | text_of
            elif .type == "queue-operation" then .content | text_of
            else empty end
            | split("<teammate-message ")[1:][]
            | select(contains("\"type\":\"idle_notification\""))
            | capture("^teammate_id=\"(?<id>[^\"]+)\"").id | "teammate:" + .;
        def teammate_woken:
            select(.type == "assistant") | .message.content[]?
            | select(.type? == "tool_use" and .name == "SendMessage")
            | .input.to | strings | "teammate:" + .;
        def process_started:
            (select(.type == "attachment" and .attachment.hookEvent? == "SessionStart"
                and ((.attachment.hookName? // "") | test("^SessionStart:(startup|resume)$"))))
            // (notification_text | select(contains("<task-notification>") and contains("__orphan_summary__")))
            | true;
        def task_events:
            (first(process_started) | {reset: true}),
            (started_id | {start: .}),
            (ended_ids | {end: .}),
            (teammate_spawned | {spawn: .}),
            (teammate_idle | {end: .}),
            (teammate_woken | {wake: .});
        [inputs | fromjson?] as $lines
        | {
            query: ((
                [ $lines[] | select(.type == "user" and .origin.kind? != "task-notification") |
                  select(([.message.content | text_of | ltrimstr(" ") | ltrimstr("\n")
                           | startswith(("<task-notification>", "<system-reminder>", "<teammate-message",
                                        "<local-command-", "<command-name>", "<command-message>",
                                        "<bash-input>", "<bash-stdout>", "<bash-stderr>"))] | any) | not) |
                  if (.message.content | type) == "string" then .
                  elif ([.message.content[]? | select(.type == "text")] | length) > 0 then .
                  else empty end
                ] | last |
                if (.message.content | type) == "array"
                then [.message.content[] | select(.type == "text") | .text] | join(" ")
                else (.message.content // "") end
            ) // ""),
            response: ((
                [$lines[] | select(.type == "assistant" and .message.content)] | last |
                [.message.content[]? | select(.type == "text") | .text] | join(" ")
            ) // ""),
            remote_url: ([$lines[] | '"$MNEMOS_JQ_REMOTE_URL"'] | last // ""),
            background_tasks: (
                reduce ($lines[] | task_events) as $e ({pending: {}, teammates: {}};
                    if $e.reset then .pending = {}
                    elif $e.start then .pending[$e.start] = true
                    elif $e.spawn then .pending[$e.spawn] = true | .teammates[$e.spawn] = true
                    elif $e.wake then (if .teammates[$e.wake] then .pending[$e.wake] = true else . end)
                    else del(.pending[$e.end]) end)
                | .pending | keys | map(select(startswith("__orphan_summary") | not)) | length
            )
          }
    ' < "$TRANSCRIPT_PATH" 2>/dev/null)

    if [ -n "$SUMMARY" ]; then
        QUERY=$(printf '%s' "$SUMMARY" | jq -r '.query')
        RESPONSE=$(printf '%s' "$SUMMARY" | jq -r '.response')
        REMOTE_URL=$(printf '%s' "$SUMMARY" | jq -r '.remote_url')
        BACKGROUND_TASKS=$(printf '%s' "$SUMMARY" | jq -r '.background_tasks')
    fi
    case "$BACKGROUND_TASKS" in ''|*[!0-9]*) BACKGROUND_TASKS=0 ;; esac

    [ ${#QUERY} -gt 200 ] && QUERY="${QUERY:0:197}..."
    [ ${#RESPONSE} -gt 200 ] && RESPONSE="${RESPONSE:0:197}..."
fi

EXTRA=()
[ -n "$REMOTE_URL" ] && EXTRA+=(--arg remote_url "$REMOTE_URL")
BODY=$(mnemos_build_payload "$INPUT" "stop" \
    --arg query "$QUERY" \
    --arg response "$RESPONSE" \
    --arg transcript_path "$TRANSCRIPT_PATH" \
    --argjson background_tasks "$BACKGROUND_TASKS" \
    "${EXTRA[@]}")
mnemos_emit "$BODY"
