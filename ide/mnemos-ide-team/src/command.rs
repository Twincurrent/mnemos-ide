// SPDX-License-Identifier: AGPL-3.0-only
//! The command line a spawned pane runs, and the environment it gets.
//!
//! The brief never goes through the shell's parser: it travels in the
//! `MNEMOS_TEAM_BRIEF` environment variable and the command line only
//! references it, quoted, so no brief can break out of the command. The other
//! arguments are validated in [`crate::names`] and contain no shell
//! metacharacters.

use crate::names::{validate_model, validate_name, NameError};

/// Environment variable that carries a spawned session's brief.
pub const BRIEF_ENV: &str = "MNEMOS_TEAM_BRIEF";
/// Environment variable that carries a spawned session's name.
pub const NAME_ENV: &str = "MNEMOS_TEAM_SESSION";

/// Which quoting rules the pane's shell uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellFamily {
    /// bash, zsh, fish and other POSIX-like shells.
    Posix,
    /// PowerShell (Windows' default in Mnemos).
    PowerShell,
}

/// What a spawn asks for, after validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub name: String,
    pub model: Option<String>,
    /// Start Claude Code with `--dangerously-skip-permissions`. Only the local,
    /// never-synced setting `agents.team.spawn_skip_permissions` turns this on.
    pub skip_permissions: bool,
}

impl LaunchSpec {
    pub fn new(
        name: &str,
        model: Option<&str>,
        skip_permissions: bool,
    ) -> Result<Self, NameError> {
        validate_name(name)?;
        if let Some(model) = model {
            validate_model(model)?;
        }
        Ok(Self {
            name: name.to_string(),
            model: model.map(str::to_string),
            skip_permissions,
        })
    }

    /// The line typed into the new pane's shell.
    pub fn command_line(&self, shell: ShellFamily) -> String {
        let mut parts = vec!["claude".to_string(), "--name".to_string(), self.name.clone()];
        if let Some(model) = &self.model {
            parts.push("--model".to_string());
            parts.push(model.clone());
        }
        if self.skip_permissions {
            parts.push("--dangerously-skip-permissions".to_string());
        }
        parts.push(match shell {
            ShellFamily::Posix => format!("\"${BRIEF_ENV}\""),
            ShellFamily::PowerShell => format!("$env:{BRIEF_ENV}"),
        });
        parts.join(" ")
    }

    /// Environment for the new pane. `USE_BUILTIN_RIPGREP=0` matches the way
    /// Claude Code is usually started on the owner's machines (the system `rg`).
    pub fn env(&self, brief: &str) -> Vec<(String, String)> {
        vec![
            (BRIEF_ENV.to_string(), brief.to_string()),
            (NAME_ENV.to_string(), self.name.clone()),
            ("USE_BUILTIN_RIPGREP".to_string(), "0".to_string()),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_command_references_the_brief() {
        let spec = LaunchSpec::new("tester", Some("sonnet"), false).unwrap();
        assert_eq!(
            spec.command_line(ShellFamily::Posix),
            "claude --name tester --model sonnet \"$MNEMOS_TEAM_BRIEF\""
        );
    }

    #[test]
    fn powershell_and_bypass() {
        let spec = LaunchSpec::new("search", None, true).unwrap();
        assert_eq!(
            spec.command_line(ShellFamily::PowerShell),
            "claude --name search --dangerously-skip-permissions $env:MNEMOS_TEAM_BRIEF"
        );
    }

    #[test]
    fn brief_stays_out_of_the_command_line() {
        let spec = LaunchSpec::new("x", None, false).unwrap();
        let brief = "\"; rm -rf ~; echo \"";
        assert!(!spec.command_line(ShellFamily::Posix).contains("rm -rf"));
        assert!(spec.env(brief).contains(&(BRIEF_ENV.to_string(), brief.to_string())));
    }

    #[test]
    fn bad_input_is_refused() {
        assert!(LaunchSpec::new("a b", None, false).is_err());
        assert!(LaunchSpec::new("ok", Some("x;y"), false).is_err());
    }
}
