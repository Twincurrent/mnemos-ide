// SPDX-License-Identifier: AGPL-3.0-only
//! Validation of what a coordinator may pass in. Everything here ends up in a
//! pane title, a branch name, a directory name or a command line, so it is kept
//! to a small, boring alphabet.

use thiserror::Error;

/// Longest brief a spawn accepts. It travels in an environment variable, and
/// Windows caps a single variable at 32 767 characters.
pub const MAX_BRIEF_CHARS: usize = 16_000;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum NameError {
    #[error("name must be 1-32 characters of a-z, 0-9 and '-', starting with a letter or digit")]
    BadName,
    #[error("model must be 1-64 characters of letters, digits, '.', '-' and '_'")]
    BadModel,
    #[error("branch must be a plain branch name (letters, digits, '.', '-', '_', '/'), at most 100 characters")]
    BadBranch,
    #[error("brief is empty")]
    EmptyBrief,
    #[error("brief is longer than {MAX_BRIEF_CHARS} characters")]
    LongBrief,
    #[error("brief contains a control character other than newline or tab")]
    BriefControlChar,
}

/// A session name: `session-tester`, `search`, `qa2`.
pub fn validate_name(name: &str) -> Result<(), NameError> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-');
    if ok {
        Ok(())
    } else {
        Err(NameError::BadName)
    }
}

/// A Claude model alias or id: `sonnet`, `opus`, `claude-sonnet-5-5`.
pub fn validate_model(model: &str) -> Result<(), NameError> {
    let ok = !model.is_empty()
        && model.len() <= 64
        && model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    if ok {
        Ok(())
    } else {
        Err(NameError::BadModel)
    }
}

/// A branch name for a new worktree. Stricter than git's own rules on purpose.
pub fn validate_branch(branch: &str) -> Result<(), NameError> {
    let ok = !branch.is_empty()
        && branch.len() <= 100
        && branch
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'/'))
        && !branch.starts_with(['-', '/', '.'])
        && !branch.ends_with(['/', '.'])
        && !branch.ends_with(".lock")
        && !branch.contains("..")
        && !branch.contains("//")
        && !branch.contains("/.");
    if ok {
        Ok(())
    } else {
        Err(NameError::BadBranch)
    }
}

/// The first prompt of a spawned session.
pub fn validate_brief(brief: &str) -> Result<(), NameError> {
    if brief.trim().is_empty() {
        return Err(NameError::EmptyBrief);
    }
    if brief.chars().count() > MAX_BRIEF_CHARS {
        return Err(NameError::LongBrief);
    }
    if brief
        .chars()
        .any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r')
    {
        return Err(NameError::BriefControlChar);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["tester", "session-search", "qa2", "a", "0x"] {
            assert_eq!(validate_name(ok), Ok(()), "{ok}");
        }
        for bad in ["", "-x", "Tester", "a b", "a/b", "a.b", &"x".repeat(33), "é"] {
            assert_eq!(validate_name(bad), Err(NameError::BadName), "{bad}");
        }
    }

    #[test]
    fn models() {
        for ok in ["sonnet", "opus", "claude-sonnet-5-5", "claude_x.1"] {
            assert_eq!(validate_model(ok), Ok(()), "{ok}");
        }
        for bad in ["", "a b", "x;rm", "$(x)", "a\"b"] {
            assert_eq!(validate_model(bad), Err(NameError::BadModel), "{bad}");
        }
    }

    #[test]
    fn branches() {
        for ok in ["feature/search", "team/tester-1", "v1.2"] {
            assert_eq!(validate_branch(ok), Ok(()), "{ok}");
        }
        for bad in ["", "-x", "/x", "x/", "a..b", "a//b", "a/.b", "x.lock", "a b", "a~b", ".x"] {
            assert_eq!(validate_branch(bad), Err(NameError::BadBranch), "{bad}");
        }
    }

    #[test]
    fn briefs() {
        assert_eq!(validate_brief("Run the search tests.\n\tThen report."), Ok(()));
        assert_eq!(validate_brief("  \n"), Err(NameError::EmptyBrief));
        assert_eq!(validate_brief("a\u{1b}[2J"), Err(NameError::BriefControlChar));
        assert_eq!(
            validate_brief(&"x".repeat(MAX_BRIEF_CHARS + 1)),
            Err(NameError::LongBrief)
        );
    }
}
