// SPDX-License-Identifier: AGPL-3.0-only
//! The team status board, `<git common dir>/mnemos-team/board.json`, which
//! the mnemos-agents plugin's `team.sh set` writes: one row per session with
//! its state, task and final SHA. Read here for the agents panel.

use std::path::Path;

use serde::Deserialize;

/// Most rows the panel shows.
pub const MAX_ROWS: usize = 40;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardRow {
    pub name: String,
    pub state: String,
    pub task: Option<String>,
    pub sha: Option<String>,
    /// UTC, as `team.sh` wrote it (`2026-10-12T15:47:07Z`).
    pub updated: Option<String>,
}

#[derive(Deserialize)]
struct Entry {
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    task: Option<String>,
    #[serde(default)]
    sha: Option<String>,
    #[serde(default)]
    updated: Option<String>,
}

/// Parses a board file's contents. Malformed input yields no rows.
pub fn parse(bytes: &[u8]) -> Vec<BoardRow> {
    let Ok(map) = serde_json::from_slice::<std::collections::BTreeMap<String, Entry>>(bytes) else {
        return Vec::new();
    };
    map.into_iter()
        .take(MAX_ROWS)
        .map(|(name, e)| BoardRow {
            name,
            state: e.state.unwrap_or_else(|| "?".to_string()),
            task: e.task.filter(|t| !t.is_empty()),
            sha: e.sha.filter(|s| !s.is_empty()),
            updated: e.updated,
        })
        .collect()
}

/// The board of the repository around `dir`, or nothing when there is none.
pub fn read(dir: &Path) -> Vec<BoardRow> {
    let Some(team) = crate::worktree::team_dir_of(dir) else {
        return Vec::new();
    };
    std::fs::read(team.join("board.json"))
        .map(|bytes| parse(&bytes))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rows_sorted_by_name() {
        let rows = parse(
            br#"{"tester":{"state":"done","task":"visual QA","updated":"2026-10-12T15:00:00Z"},
                 "builder":{"state":"review","task":"","sha":"abc1234"}}"#,
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].name, "builder");
        assert_eq!(rows[0].task, None);
        assert_eq!(rows[0].sha.as_deref(), Some("abc1234"));
        assert_eq!(rows[1].task.as_deref(), Some("visual QA"));
    }

    #[test]
    fn bad_input_is_empty() {
        assert!(parse(b"not json").is_empty());
        assert!(parse(b"[]").is_empty());
        assert!(read(Path::new("/definitely/not/a/repo")).is_empty());
    }
}
