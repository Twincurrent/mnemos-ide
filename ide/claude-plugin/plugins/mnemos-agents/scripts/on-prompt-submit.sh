#!/bin/bash
# SPDX-License-Identifier: MIT OR AGPL-3.0-only
# UserPromptSubmit -> emit "prompt_submit" (back to running; carries the prompt
# and, once Remote Control has connected, the session's claude.ai URL).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

mnemos_should_emit || exit 0
command -v jq >/dev/null 2>&1 || exit 0

INPUT=$(cat)
QUERY=$(printf '%s' "$INPUT" | jq -r '.prompt // empty' 2>/dev/null)
# A turn Claude Code starts itself (a background task reporting back, a teammate
# message) is not the user's prompt: send no query, so Mnemos keeps the last one.
case "${QUERY#"${QUERY%%[![:space:]]*}"}" in
    "<task-notification>"*|"<system-reminder>"*|"<teammate-message"*|"<local-command-"*|"<command-name>"*|"<command-message>"*|"<bash-input>"*|"<bash-stdout>"*|"<bash-stderr>"*) QUERY="" ;;
esac
[ ${#QUERY} -gt 200 ] && QUERY="${QUERY:0:197}..."

REMOTE_URL=$(mnemos_remote_url "$(printf '%s' "$INPUT" | jq -r '.transcript_path // empty' 2>/dev/null)")

EXTRA=()
[ -n "$REMOTE_URL" ] && EXTRA+=(--arg remote_url "$REMOTE_URL")
BODY=$(mnemos_build_payload "$INPUT" "prompt_submit" --arg query "$QUERY" "${EXTRA[@]}")
mnemos_emit "$BODY"
