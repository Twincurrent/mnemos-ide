#!/bin/bash
# SPDX-License-Identifier: MIT OR AGPL-3.0-only
# PermissionRequest -> emit "permission_request" (blocked, needs approval).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

mnemos_should_emit || exit 0
command -v jq >/dev/null 2>&1 || exit 0

INPUT=$(cat)
TOOL_NAME=$(printf '%s' "$INPUT" | jq -r '.tool_name // "unknown"' 2>/dev/null)
TOOL_INPUT=$(printf '%s' "$INPUT" | jq -c '.tool_input // {}' 2>/dev/null)
[ -z "$TOOL_INPUT" ] && TOOL_INPUT='{}'

TOOL_PREVIEW=$(printf '%s' "$INPUT" | jq -r '(.tool_input | if .command then .command elif .file_path then .file_path else (tostring | .[0:80]) end) // ""' 2>/dev/null)
SUMMARY="Wants to run $TOOL_NAME"
if [ -n "$TOOL_PREVIEW" ]; then
    [ ${#TOOL_PREVIEW} -gt 120 ] && TOOL_PREVIEW="${TOOL_PREVIEW:0:117}..."
    SUMMARY="$SUMMARY: $TOOL_PREVIEW"
fi

BODY=$(mnemos_build_payload "$INPUT" "permission_request" \
    --arg summary "$SUMMARY" \
    --arg tool_name "$TOOL_NAME" \
    --argjson tool_input "$TOOL_INPUT")
mnemos_emit "$BODY"
