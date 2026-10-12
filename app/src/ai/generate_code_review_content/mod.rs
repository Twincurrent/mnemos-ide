// TODO(edward): follow-up — gate callers of this module on both
//   1. an `AISettings` opt-out (like `is_git_operations_autogen_enabled`), and
//   2. a customer-type guard (exclude Enterprise unless Mnemos plan / dogfood),
// matching the pattern in `code_review/git_dialog/mod.rs::should_send_git_ops_ai_request`.
// `FeatureFlag::GitOperationsInCodeReview` already gates the surrounding UI,
// but does not address AI-specific privacy / opt-out concerns for sending
// diffs to an LLM.
pub(crate) mod api;
