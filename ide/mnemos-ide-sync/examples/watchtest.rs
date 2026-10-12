// SPDX-License-Identifier: AGPL-3.0-only
//! Manual watcher smoke test: `cargo run -p mnemos-ide-sync --example watchtest`
//! then touch ~/.claude/CLAUDE.md in another shell and watch for events.

use std::path::PathBuf;

use mnemos_ide_sync::watcher::WatchTarget;

#[tokio::main]
async fn main() {
    let home = std::env::var("HOME").unwrap();
    let claude = PathBuf::from(&home).join(".claude");
    let paths = vec![
        WatchTarget::file(claude.join("CLAUDE.md")),
        WatchTarget::dir(claude.join("agents")),
        WatchTarget::dir(claude.join("rules")),
    ];
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let _w = mnemos_ide_sync::watcher::SyncWatcher::new(&paths, tx).expect("watcher");
    eprintln!("watching {paths:?}; modify a file now");
    while let Some(ev) = rx.recv().await {
        eprintln!("EVENT: {:?} remove={}", ev.paths, ev.is_remove);
    }
}
