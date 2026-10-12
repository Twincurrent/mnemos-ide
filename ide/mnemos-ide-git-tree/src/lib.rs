// SPDX-License-Identifier: AGPL-3.0-only
//! Mnemos IDE Git Tree: branch/commit-graph panel for the code review view.
//!
//! A dropdown in the code review header, replacing Warp's `DiffSelector`:
//! branches, remotes, tags and a multi-branch commit graph.
//!
//! - Clicking a commit selects it and emits `GitTreeEvent::CommitSelected`,
//!   which loads its diff.
//! - Branch and tag rows, and a selected commit's chips, emit
//!   `CheckoutBranch` / `CheckoutCommit` / `CreateBranchAt`. The app runs
//!   those through `GitGraphModel` with libgit2's *safe* checkout, which
//!   refuses rather than discarding uncommitted work.
//!
//! The app builds a `GitGraphModel` off the UI thread whenever the repo
//! changes, calls `refresh()`, runs `compute_layout` over `commits()`, and
//! pushes the result into `GitTreeView::set_layout` plus `refs()` into
//! `GitTreeView::set_refs`. Checkout methods do not refresh the model; the
//! caller re-syncs afterwards.
//!
//! Open pull requests come from `gh`, run by the app; `prs` parses its
//! output and decides what a click on a PR row does, and the app pushes the
//! result into `GitTreeView::set_pull_requests`.

#![forbid(unsafe_code)]
// Mnemos IDE: alongside the forbid above. If unsafe is ever allowed in this crate, every
// unsafe block needs a `// SAFETY:` comment.
#![deny(clippy::undocumented_unsafe_blocks)]

pub mod lane_router;
pub mod model;
pub mod prs;
pub mod view;

pub use lane_router::{compute_layout, Edge, EdgeKind, GraphLayout};
pub use model::{CommitNode, GitError, GitGraphModel, RefKind, RefNode};
pub use prs::{
    is_github_pr_url, is_valid_branch_name, origin_github_slug, parse_pr_list, PrSummary,
    PR_LIST_FIELDS,
};
pub use view::{GitTreeEvent, GitTreeView};
