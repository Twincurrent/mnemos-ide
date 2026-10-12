#!/bin/bash
# SPDX-License-Identifier: MIT OR AGPL-3.0-only
# SessionStart -> emit "session_start" (binds session_id + cwd to the pane).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/lib.sh"

mnemos_should_emit || exit 0
command -v jq >/dev/null 2>&1 || exit 0

INPUT=$(cat)

# Report the Mnemos-compat protocol version (>= Mnemos MINIMUM_PLUGIN_VERSION),
# not plugin.json's release number — see MNEMOS_REPORTED_PLUGIN_VERSION in lib.sh.
# On a resume the transcript already has the Remote Control URL; on a fresh
# start it usually does not yet, and the first prompt_submit carries it.
REMOTE_URL=$(mnemos_remote_url "$(printf '%s' "$INPUT" | jq -r '.transcript_path // empty' 2>/dev/null)")

EXTRA=()
[ -n "$REMOTE_URL" ] && EXTRA+=(--arg remote_url "$REMOTE_URL")
BODY=$(mnemos_build_payload "$INPUT" "session_start" --arg plugin_version "$MNEMOS_REPORTED_PLUGIN_VERSION" "${EXTRA[@]}")
mnemos_emit "$BODY"
