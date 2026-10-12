#!/bin/bash
# SPDX-License-Identifier: MIT OR AGPL-3.0-only
# Shared helpers for the Mnemos CLI-agent plugin.
#
# Emits structured OSC 777 "cli-agent" events that the Mnemos terminal parses
# (app/src/terminal/cli_agent_sessions/event/). Each event carries the session
# cwd + conversation context so a running `claude` attaches to its pane in the
# Mnemos agents panel instead of showing "Unattached".
#
# WHY THIS PLUGIN EXISTS (vs the upstream warp@claude-code-warp plugin):
#   Claude Code runs hooks as detached subprocesses with NO controlling terminal
#   and stdio piped back to the agent — so the upstream plugin's
#   `printf ... > /dev/tty` write fails with ENXIO ("Device not configured")
#   and is silently swallowed by `|| true`. Zero bytes ever reach the PTY that
#   Mnemos parses, so every session stays Unattached.
#   We instead resolve claude's real pty by walking up the process tree and
#   write the OSC straight to that device, which delivers to the Mnemos master.

# Mnemos trusts a cli-agent event only when its title carries the token it
# exported to this terminal (MNEMOS_CLI_AGENT_TOKEN), so output that merely
# prints an OSC 777 sequence cannot fake one. Without the token (an older
# Mnemos, or not a Mnemos terminal) the bare sentinel is sent and ignored.
MNEMOS_SENTINEL="mnemos://cli-agent${MNEMOS_CLI_AGENT_TOKEN:+#$MNEMOS_CLI_AGENT_TOKEN}"
MNEMOS_PLUGIN_PROTOCOL_VERSION=1

# Version advertised to Mnemos in the event `plugin_version` field. Mnemos
# compares this against its MINIMUM_PLUGIN_VERSION (the upstream warp plugin's
# scheme, currently 2.0.0) and shows a spurious "Update plugin" chip when it is
# lower. We therefore track the cli-agent *protocol generation* here, not this
# plugin's own release number (that lives in plugin.json). Keep this >= Mnemos's
# MINIMUM_PLUGIN_VERSION.
MNEMOS_REPORTED_PLUGIN_VERSION="2.1.0"

# Returns 0 when we're inside a Mnemos terminal that advertises the event protocol.
mnemos_should_emit() {
    # Mnemos sets MNEMOS_TERMINAL=1 (and mirrors WARP_TERMINAL=1).
    [ -n "${MNEMOS_TERMINAL:-}${WARP_TERMINAL:-}" ] || return 1
    # Mnemos only exports the protocol version when its event infra is live.
    [ -n "${MNEMOS_CLI_AGENT_PROTOCOL_VERSION:-}${WARP_CLI_AGENT_PROTOCOL_VERSION:-}" ] || return 1
    return 0
}

# Resolve the controlling pty device of the claude session.
# Hooks have no ctty, so we walk up from $PPID to the first ancestor (claude
# itself) that owns a real tty, and return its /dev path if writable.
_mnemos_pty_device() {
    local pid="$PPID" tname dev i=0
    while [ -n "$pid" ] && [ "$pid" != "0" ] && [ "$pid" != "1" ] && [ "$i" -lt 16 ]; do
        tname=$(ps -o tty= -p "$pid" 2>/dev/null | tr -d '[:space:]')
        case "$tname" in
            /dev/*)        dev="$tname" ;;
            ttys[0-9]*)    dev="/dev/$tname" ;;
            tty[0-9A-Za-z]*) dev="/dev/$tname" ;;
            s[0-9]*)       dev="/dev/tty$tname" ;;   # abbreviated form -> ttysNNN
            ""|"?"|"??")   dev="" ;;
            *)             dev="/dev/$tname" ;;
        esac
        if [ -n "$dev" ] && [ -w "$dev" ]; then
            printf '%s' "$dev"
            return 0
        fi
        pid=$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d '[:space:]')
        i=$((i + 1))
    done
    return 1
}

# Emit an OSC 777 cli-agent notification carrying the JSON body in $1.
#
# Delivery order (exactly one path succeeds, so events are never doubled):
#   1. /dev/tty           — if the hook somehow retained a controlling terminal.
#   2. claude's pty device — resolved by walking up the process tree. This is
#      the proven path under Claude Code, which runs hooks detached.
#   3. terminalSequence    — Claude Code's official hook-output field
#      (CC >= 2.1.141): the OSC rides the hook's stdout JSON and Claude Code
#      writes it to the terminal. Used only when no pty device is reachable.
# Each direct-write is wrapped in a `{ ...; } 2>/dev/null` group so a failed
# redirection (ENXIO when there is no controlling tty) is swallowed instead of
# leaking to the hook's stderr.
mnemos_emit() {
    local body="$1" seq dev
    seq=$(printf '\033]777;notify;%s;%s\007' "$MNEMOS_SENTINEL" "$body")

    if { printf '%s' "$seq" >/dev/tty; } 2>/dev/null; then
        return 0
    fi

    dev=$(_mnemos_pty_device)
    if [ -n "$dev" ] && { printf '%s' "$seq" >"$dev"; } 2>/dev/null; then
        return 0
    fi

    # Fallback: hand the sequence to Claude Code via its structured output.
    jq -nc --arg seq "$seq" '{terminalSequence: $seq}'
}

# jq filter: from one transcript line, the claude.ai Remote Control URL of a
# `remote_session_change` attachment, if it has a valid one. Mnemos opens this
# URL on the user's other machines, so anything that is not exactly a
# claude.ai session URL is dropped here (and again in Mnemos).
MNEMOS_JQ_REMOTE_URL='select(.type == "attachment" and .attachment.type? == "remote_session_change")
    | .attachment.url | strings
    | select(test("^https://claude\\.ai/code/session_[A-Za-z0-9_-]+$"))'

# Print the session's Remote Control URL from the transcript at $1: the last
# valid one, since Remote Control can reconnect under a new URL. Prints
# nothing when there is none. Only lines that mention the attachment are
# parsed, and a half-written last line is skipped, not fatal.
mnemos_remote_url() {
    local transcript="$1"
    [ -n "$transcript" ] && [ -f "$transcript" ] || return 0
    grep -F '"remote_session_change"' "$transcript" 2>/dev/null \
        | jq -nRr "[inputs | fromjson? | $MNEMOS_JQ_REMOTE_URL] | last // empty" 2>/dev/null
}

# Build the structured JSON payload.
# Usage: mnemos_build_payload <stdin_json> <event> [extra jq --arg/--argjson pairs...]
mnemos_build_payload() {
    local input="$1" event="$2"
    shift 2
    local declared="${MNEMOS_CLI_AGENT_PROTOCOL_VERSION:-${WARP_CLI_AGENT_PROTOCOL_VERSION:-1}}"
    local v="$MNEMOS_PLUGIN_PROTOCOL_VERSION"
    # Negotiate min(plugin, terminal-declared); fall back to the plugin version on garbage.
    if [ "$declared" -lt "$MNEMOS_PLUGIN_PROTOCOL_VERSION" ] 2>/dev/null; then
        v="$declared"
    fi
    local session_id cwd project=""
    session_id=$(printf '%s' "$input" | jq -r '.session_id // empty' 2>/dev/null)
    cwd=$(printf '%s' "$input" | jq -r '.cwd // empty' 2>/dev/null)
    [ -n "$cwd" ] && project=$(basename "$cwd")
    jq -nc \
        --argjson v "$v" \
        --arg agent "claude" \
        --arg event "$event" \
        --arg session_id "$session_id" \
        --arg cwd "$cwd" \
        --arg project "$project" \
        "$@" \
        '{v:$v, agent:$agent, event:$event, session_id:$session_id, cwd:$cwd, project:$project} + $ARGS.named'
}
