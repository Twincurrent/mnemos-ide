#!/bin/bash
# SPDX-License-Identifier: MIT OR AGPL-3.0-only
# PostToolUse -> emit "tool_complete" (back to running after a tool call).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

mnemos_should_emit || exit 0
command -v jq >/dev/null 2>&1 || exit 0

INPUT=$(cat)
TOOL_NAME=$(printf '%s' "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)

BODY=$(mnemos_build_payload "$INPUT" "tool_complete" --arg tool_name "$TOOL_NAME")
mnemos_emit "$BODY"
