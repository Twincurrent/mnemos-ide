#!/bin/bash
# SPDX-License-Identifier: MIT OR AGPL-3.0-only
# Notification (idle_prompt) -> emit the notification_type (e.g. "idle_prompt").
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

mnemos_should_emit || exit 0
command -v jq >/dev/null 2>&1 || exit 0

INPUT=$(cat)
NOTIF_TYPE=$(printf '%s' "$INPUT" | jq -r '.notification_type // "idle_prompt"' 2>/dev/null)
MSG=$(printf '%s' "$INPUT" | jq -r '.message // "Input needed"' 2>/dev/null)
[ -z "$MSG" ] && MSG="Input needed"

BODY=$(mnemos_build_payload "$INPUT" "$NOTIF_TYPE" --arg summary "$MSG")
mnemos_emit "$BODY"
