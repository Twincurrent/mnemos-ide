#!/bin/bash
# SPDX-License-Identifier: MIT OR AGPL-3.0-only
# Shared rules and a status board for several Claude Code sessions working on
# one repository (a coordinator plus workers, each in its own worktree).
#
# Both live in the repository's common git dir, so every worktree of the repo
# sees the same files and nothing is ever committed:
#   <git common dir>/mnemos-team/rules.md     what every session must follow
#   <git common dir>/mnemos-team/board.json   who does what, state, final SHA
# MNEMOS_TEAM_DIR overrides the location.
#
# on-team-context.sh feeds a changed rules file and board into each session's
# next turn. This script is also the command line the sessions use:
#   team.sh dir                                   print the team dir (creating it)
#   team.sh rules                                 print the rules file
#   team.sh board                                 print the board
#   team.sh set <name> <state> [task] [sha]       add or update one board row
#   team.sh remove <name>                         drop a board row
# States are free text; idle, working, blocked, review and done read best.

# The team dir for the repository around $1 (default: the current dir), or
# MNEMOS_TEAM_DIR. Prints nothing and fails outside a git repository.
mnemos_team_dir() {
    if [ -n "${MNEMOS_TEAM_DIR:-}" ]; then
        printf '%s' "$MNEMOS_TEAM_DIR"
        return 0
    fi
    local common
    common=$(git -C "${1:-.}" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || return 1
    [ -n "$common" ] || return 1
    printf '%s/mnemos-team' "$common"
}

# A content checksum for change detection; empty for a missing file.
mnemos_team_sum() {
    [ -f "$1" ] || return 0
    cksum < "$1" | awk '{print $1 "-" $2}'
}

# Runs "$@" while holding the board lock (a directory, since macOS has no
# flock). A lock older than 30 s is taken over: its holder died.
mnemos_team_locked() {
    local lock="$TEAM_DIR/.board.lock" i=0
    while ! mkdir "$lock" 2>/dev/null; do
        i=$((i + 1))
        if [ "$i" -ge 150 ]; then
            rm -rf "$lock"
            i=0
            continue
        fi
        sleep 0.2
    done
    "$@"
    local rc=$?
    rmdir "$lock" 2>/dev/null
    return $rc
}

_mnemos_team_write_board() {
    local board="$TEAM_DIR/board.json" tmp
    [ -f "$board" ] || printf '{}\n' > "$board"
    tmp=$(mktemp "$TEAM_DIR/.board.XXXXXX") || return 1
    if jq "$@" "$board" > "$tmp"; then
        mv "$tmp" "$board"
    else
        rm -f "$tmp"
        return 1
    fi
}

mnemos_team_main() {
    command -v jq >/dev/null 2>&1 || { echo "team.sh: jq is required" >&2; return 1; }
    TEAM_DIR=$(mnemos_team_dir) || { echo "team.sh: not inside a git repository" >&2; return 1; }
    mkdir -p "$TEAM_DIR" || return 1
    local cmd="${1:-}"
    shift || true
    case "$cmd" in
        dir) printf '%s\n' "$TEAM_DIR" ;;
        rules) [ -f "$TEAM_DIR/rules.md" ] && cat "$TEAM_DIR/rules.md" ;;
        board) if [ -f "$TEAM_DIR/board.json" ]; then cat "$TEAM_DIR/board.json"; else printf '{}\n'; fi ;;
        set)
            [ $# -ge 2 ] || { echo "usage: team.sh set <name> <state> [task] [sha]" >&2; return 2; }
            mnemos_team_locked _mnemos_team_write_board \
                --arg n "$1" --arg s "$2" --arg t "${3:-}" --arg h "${4:-}" \
                --arg at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
                '.[$n] = ((.[$n] // {}) + {state: $s, updated: $at}
                    + (if $t != "" then {task: $t} else {} end)
                    + (if $h != "" then {sha: $h} else {} end))'
            ;;
        remove)
            [ $# -ge 1 ] || { echo "usage: team.sh remove <name>" >&2; return 2; }
            mnemos_team_locked _mnemos_team_write_board --arg n "$1" 'del(.[$n])'
            ;;
        *) sed -n '2,20p' "${BASH_SOURCE[0]}" >&2; return 2 ;;
    esac
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    mnemos_team_main "$@"
fi
