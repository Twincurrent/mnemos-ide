#!/bin/bash
# SPDX-License-Identifier: MIT OR AGPL-3.0-only
# SessionStart / UserPromptSubmit -> add the team rules and board (team.sh) to
# the session's context, but only when they changed since this session last
# saw them, so a coordinator edits one file instead of messaging every session.
# Silent outside a git repository and in a repository with no team files, so it
# costs nothing in ordinary use. Works in any terminal, not only Mnemos.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/team.sh"

command -v jq >/dev/null 2>&1 || exit 0

INPUT=$(cat)
EVENT=$(printf '%s' "$INPUT" | jq -r '.hook_event_name // empty' 2>/dev/null)
SESSION=$(printf '%s' "$INPUT" | jq -r '.session_id // empty' 2>/dev/null)
CWD=$(printf '%s' "$INPUT" | jq -r '.cwd // empty' 2>/dev/null)
[ -n "$EVENT" ] && [ -n "$SESSION" ] || exit 0
case "$SESSION" in *[!A-Za-z0-9_-]*) exit 0 ;; esac

TEAM_DIR=$(mnemos_team_dir "${CWD:-.}") || exit 0
RULES="$TEAM_DIR/rules.md"
BOARD="$TEAM_DIR/board.json"
[ -f "$RULES" ] || [ -f "$BOARD" ] || exit 0

# The team.sh sessions should run: the installed copy of this plugin version
# when there is one (a directory marketplace can run hooks from the source
# checkout, which may be on another branch), else this script's own.
TEAM_SH="$SCRIPT_DIR/team.sh"
VERSION=$(jq -r '.version // empty' "$SCRIPT_DIR/../.claude-plugin/plugin.json" 2>/dev/null)
INSTALLED="${HOME}/.claude/plugins/cache/mnemos/mnemos-agents/${VERSION}/scripts/team.sh"
if [ -n "$VERSION" ] && [ -f "$INSTALLED" ]; then
    TEAM_SH="$INSTALLED"
fi

RULES_SUM=$(mnemos_team_sum "$RULES")
BOARD_SUM=$(mnemos_team_sum "$BOARD")
SEEN_FILE="$TEAM_DIR/seen/$SESSION"
SEEN_RULES=""
SEEN_BOARD=""
if [ -f "$SEEN_FILE" ]; then
    read -r SEEN_RULES SEEN_BOARD < "$SEEN_FILE"
fi
# After a compaction the session has lost what it was shown: show everything.
if [ "$EVENT" = "SessionStart" ]; then
    SEEN_RULES=""
    SEEN_BOARD=""
fi

CONTEXT=""
if [ -n "$RULES_SUM" ] && [ "$RULES_SUM" != "$SEEN_RULES" ]; then
    # Capped so a runaway file cannot flood every session.
    CONTEXT+="Team rules (${RULES}; they apply to every session on this repository and override earlier instructions from other sessions; the coordinator edits them, not you):"$'\n'
    CONTEXT+="$(head -c 8000 "$RULES")"$'\n\n'
fi
if [ -n "$BOARD_SUM" ] && [ "$BOARD_SUM" != "$SEEN_BOARD" ]; then
    LINES=$(jq -r 'to_entries | sort_by(.key) | .[] |
        "- \(.key): \(.value.state // "?")\(if .value.task then " | \(.value.task)" else "" end)\(if .value.sha then " | sha \(.value.sha)" else "" end) (\(.value.updated // "?"))"' \
        "$BOARD" 2>/dev/null | head -n 40)
    if [ -n "$LINES" ]; then
        CONTEXT+="Team board (${BOARD}). Update your own row with: bash \"${TEAM_SH}\" set <your name> <state> [task] [sha]"$'\n'
        CONTEXT+="$LINES"$'\n'
    fi
fi

mkdir -p "$TEAM_DIR/seen" 2>/dev/null && printf '%s %s\n' "$RULES_SUM" "$BOARD_SUM" > "$SEEN_FILE"

[ -n "$CONTEXT" ] || exit 0
jq -nc --arg e "$EVENT" --arg c "$CONTEXT" \
    '{hookSpecificOutput: {hookEventName: $e, additionalContext: $c}}'
