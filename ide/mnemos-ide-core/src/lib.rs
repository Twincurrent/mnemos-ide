// SPDX-License-Identifier: AGPL-3.0-only
//! mnemos-ide-core: foundation types, paths, and errors shared across all mnemos-ide-* crates.
//!
//! Pure data + path helpers. Zero async, no I/O beyond the create-if-missing
//! data-dir setup in `paths`. `registry` holds the cross-machine merge rules
//! for the synced `projects.json`.

#![forbid(unsafe_code)]
// Mnemos IDE: alongside the forbid above. If unsafe is ever allowed in this crate, every
// unsafe block needs a `// SAFETY:` comment.
#![deny(clippy::undocumented_unsafe_blocks)]

pub mod error;
pub mod paths;
pub mod registry;
pub mod types;

pub use error::IdeError;
pub use types::*;
