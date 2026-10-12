// SPDX-License-Identifier: AGPL-3.0-only
//! Mnemos IDE Projects: the Project model, its on-disk store, and the in-memory
//! `ProjectTabs` state (open projects + active id) the workspace keeps
//! alongside its native tabs.
//!
//! In Mnemos IDE every native workspace tab is a project, so the native tab strip
//! is what the user clicks. The former rendered `IdeProjectTabBar` was removed
//! on 2026-09-02; only its model survives as `ProjectTabs`.
//!
//! Cross-machine: `resolve` finds a project's checkout on this machine and
//! `reconcile` plans the tab strip so every machine shows the registry's open
//! set. The merge rules themselves live in `mnemos_ide_core::registry`.

#![forbid(unsafe_code)]
// Mnemos IDE: alongside the forbid above. If unsafe is ever allowed in this crate, every
// unsafe block needs a `// SAFETY:` comment.
#![deny(clippy::undocumented_unsafe_blocks)]

pub mod project_tabs;
pub mod reconcile;
pub mod resolve;
pub mod store;

pub use project_tabs::ProjectTabs;
pub use reconcile::{plan, OpenTab, ReconcileMode, ReconcilePlan, TabSnapshot};
pub use resolve::{tab_root, FsEnv, LocalResolver, RealFs};
pub use store::{ProjectStore, TabState};

// Re-export the canonical types from mnemos-ide-core so downstream crates that only
// touch projects don't need to depend on mnemos-ide-core directly.
pub use mnemos_ide_core::registry;
pub use mnemos_ide_core::types::Project;
pub use mnemos_ide_core::IdeError;
