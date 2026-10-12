// SPDX-License-Identifier: AGPL-3.0-only
//! Running agents across machines, for the agents panel.
//!
//! Each Mnemos instance publishes the agents it runs under its own state key,
//! `agents-live:<machine>`, last write wins, and reads every machine's key back
//! with a prefix query. A machine that stops publishing (closed, asleep, offline)
//! drops out once its snapshot is older than [`STALE_AFTER`]. The state is per
//! admin profile, so it is never shared with other users or a team.
//!
//! Nothing here touches disk: `apply_state` skips the prefix, and the panel
//! keeps the remote snapshots in memory only.
//!
//! Privacy: the app publishes only while the user's live-feed setting is on.
//! What goes out is passed through [`MachineAgents::for_publish`]: prompt and
//! reply text only when the user opted in to sharing it, and every text field
//! through [`redact_secrets`] either way. The app deletes this machine's key
//! when the setting goes off and on sign-out
//! ([`SyncClient::delete_live_agents`]), and the server drops keys that have
//! not been written for 24 hours.
//!
//! The key suffix stays `machine_key(host)`: other readers (Iris) find "this
//! machine" by it. The random machine id ([`crate::machine`]) rides along as
//! the optional `machine_id` field.
//!
//! The agents panel's "Suggest" no longer goes through the server: the app
//! runs the user's own `claude` CLI for it (`app/src/workspace/view/agent_suggest.rs`).

use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::client::{PullAllResponse, SyncClient, SYNC_API};
use crate::error::IdeError;

pub const KEY_PREFIX: &str = "agents-live:";
/// A snapshot older than this is a machine that went away.
pub const STALE_AFTER: chrono::Duration = chrono::Duration::minutes(3);
/// Caps, so one machine with a runaway list cannot bloat the key.
const MAX_AGENTS: usize = 40;
const MAX_TEXT_CHARS: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineAgents {
    /// The key suffix; `machine_key` of the host name.
    pub machine: String,
    /// What to show: the host name as the machine reports it.
    pub label: String,
    /// `darwin`, `win32` or `linux`.
    pub platform: String,
    pub updated_at: DateTime<Utc>,
    pub agents: Vec<LiveAgent>,
    /// The random machine id transcript sync also uses ([`crate::machine`]).
    /// Optional: older clients neither send nor read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveAgent {
    /// The Claude Code session id when known: stable across restarts and
    /// unique per agent, unlike the project. Other readers (Iris) key on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub project: String,
    /// `claude`, `codex`, ...
    pub agent: String,
    /// `blocked`, `running` or `idle`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity: Option<DateTime<Utc>>,
    /// The session's claude.ai Remote Control URL, so another machine can
    /// open it. Readers validate it before use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    /// Background tasks still running after the turn ended (status is then
    /// `running`, and `tool` says "N background tasks" for older readers).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub background_tasks: u32,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl MachineAgents {
    /// Cap the list and its texts before publishing.
    pub fn capped(mut self) -> Self {
        self.agents.truncate(MAX_AGENTS);
        for a in &mut self.agents {
            for t in [&mut a.query, &mut a.response, &mut a.tool]
                .into_iter()
                .flatten()
            {
                if t.chars().count() > MAX_TEXT_CHARS {
                    *t = t.chars().take(MAX_TEXT_CHARS).collect();
                }
            }
            // A cut URL is a wrong URL: drop an oversized one instead.
            if a.remote_url
                .as_ref()
                .is_some_and(|u| u.len() > MAX_TEXT_CHARS)
            {
                a.remote_url = None;
            }
        }
        self
    }

    /// What this machine may publish: prompt and reply text only when
    /// `share_text` (the user opted in), and every remaining text field with
    /// secrets redacted. Capped as well.
    pub fn for_publish(mut self, share_text: bool) -> Self {
        for a in &mut self.agents {
            if !share_text {
                a.query = None;
                a.response = None;
            }
            for t in [&mut a.query, &mut a.response, &mut a.tool]
                .into_iter()
                .flatten()
            {
                *t = redact_secrets(t);
            }
            a.project = redact_secrets(&a.project);
        }
        self.capped()
    }

    pub fn is_stale(&self, now: DateTime<Utc>) -> bool {
        now.signed_duration_since(self.updated_at) > STALE_AFTER
    }
}

const REDACTED: &str = "[redacted]";

/// Secret-looking substrings: private key blocks, `Bearer`/`Basic`
/// credentials, `Authorization` and `Cookie` headers, well-known token formats
/// (AWS, GitHub, GitLab, Slack, Stripe, Google, OpenAI, Anthropic, Resend,
/// npm, Hugging Face, SendGrid, JWTs), Slack and Discord webhook URLs, URLs
/// with a password, password flags (`mysql -pX`, `--password X`), long hex
/// strings (48+ digits: R2 and similar secrets; a git SHA is 40 and stays),
/// and `key=value` / `key: value` pairs whose key names a secret (password,
/// pass, token, api key...), quoted values with spaces included.
///
/// A pattern with a group named `s` replaces only that group (the flag or
/// header name stays readable); any other replaces its whole match.
static SECRET_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        // A PEM private key, whole or cut off by the text cap.
        r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----(?s:.*?)(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)",
        r"(?i)\bbearer\s+[A-Za-z0-9\-._~+/]{12,}=*",
        r"\bBasic\s+[A-Za-z0-9+/]{12,}={0,2}",
        r"\b(?:AKIA|ASIA|AGPA|AIDA|AROA|ANPA|ANVA|AIPA)[A-Z0-9]{16}\b",
        r"\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{20,}\b",
        r"\bgithub_pat_[A-Za-z0-9_]{20,}\b",
        r"\bglpat-[A-Za-z0-9\-_]{20,}\b",
        r"\bxox[abposr]-[A-Za-z0-9-]{10,}\b",
        r"\b(?:sk|rk|pk)_(?:live|test)_[A-Za-z0-9]{10,}\b",
        r"\bsk-(?:ant-|proj-)?[A-Za-z0-9\-_]{16,}\b",
        r"\bAIza[0-9A-Za-z\-_]{35}\b",
        r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
        // Resend, npm, Hugging Face, SendGrid.
        r"\bre_[A-Za-z0-9]{6,}_[A-Za-z0-9]{16,}\b",
        r"\bnpm_[A-Za-z0-9]{30,}\b",
        r"\bhf_[A-Za-z0-9]{30,}\b",
        r"\bSG\.[A-Za-z0-9_\-]{16,}\.[A-Za-z0-9_\-]{16,}",
        // Webhook URLs carry their secret in the path.
        r"(?i)https://hooks\.slack\.com/(?:services|workflows|triggers)/[A-Za-z0-9/_\-]+",
        r"(?i)https://(?:ptb\.|canary\.)?discord(?:app)?\.com/api/webhooks/[0-9]+/[A-Za-z0-9_\-]+",
        // Any `Authorization` header value (`token <hex>`, `Basic`, custom
        // schemes) and every cookie.
        r"(?i)\bauthorization\s*:\s*(?:[A-Za-z]+\s+)?(?P<s>[A-Za-z0-9\-._~+/=]{8,})",
        r"(?i)\b(?:set-)?cookie\s*:\s*(?P<s>[^\n]+)",
        // Password flags: `mysql -pXXXX` (glued to the flag), `--password X`.
        r#"(?i)\b(?:mysql|mysqldump|mysqladmin|mariadb|mariadb-dump)\b[^\n]*?\s-p(?P<s>[^\s'"]+)"#,
        r#"(?i)--password(?:=|\s+)(?P<s>[^\s'"]+)"#,
        // `password=...`, `DB_PASS=...`, `api_key: ...`, `"secret": "..."`,
        // `AWS_SECRET_ACCESS_KEY=...`, `PASSWORD="a quoted phrase"`.
        r#"(?i)[A-Za-z0-9_\-]*(?:password|passwd|pass|pwd|secret|token|api[_\-]?key|access[_\-]?key|private[_\-]?key|credentials?)[A-Za-z0-9_\-]*["']?\s*[:=]\s*(?:"[^"\n]+"|'[^'\n]+'|[^\s"',;]{4,})"#,
        // Long bare hex: R2/S3 secret keys (64), SHA-256 digests.
        r"\b[0-9a-fA-F]{48,}\b",
        // A URL with a password in it: `https://user:pass@host`.
        r"(?i)\b[a-z][a-z0-9+.\-]*://[^\s/:@]+:[^\s/@]+@",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("secret pattern compiles"))
    .collect()
});

/// `text` with every secret-looking substring replaced by `[redacted]`.
/// Applied to everything the live feed publishes, whatever the user's
/// sharing setting.
pub fn redact_secrets(text: &str) -> String {
    let mut out = text.to_string();
    for re in SECRET_PATTERNS.iter() {
        if re.is_match(&out) {
            out = re
                .replace_all(&out, |caps: &regex::Captures<'_>| {
                    let whole = caps.get(0).expect("group 0 always matches");
                    match caps.name("s") {
                        Some(secret) => format!(
                            "{}{}{}",
                            &whole.as_str()[..secret.start() - whole.start()],
                            REDACTED,
                            &whole.as_str()[secret.end() - whole.start()..],
                        ),
                        None => REDACTED.to_string(),
                    }
                })
                .into_owned();
        }
    }
    out
}

/// A host name as a key suffix: lowercase ASCII letters, digits and `-`.
pub fn machine_key(host: &str) -> String {
    let key: String = host
        .trim()
        .trim_end_matches(".local")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .take(63)
        .collect();
    if key.trim_matches('-').is_empty() {
        "unknown".to_string()
    } else {
        key
    }
}

/// Keep the other machines' fresh snapshots: drop this machine, stale ones and
/// anything that does not parse. This machine is its key (`this_machine`)
/// and also any snapshot carrying its id (`this_machine_id`): a host name
/// can change under a running Mac (another network, a wake from sleep), and
/// the previous instance's snapshot under the old name is still this
/// machine, not another one running its sessions.
pub fn others(
    states: impl IntoIterator<Item = (String, serde_json::Value)>,
    this_machine: &str,
    this_machine_id: Option<&str>,
    now: DateTime<Utc>,
) -> Vec<MachineAgents> {
    let mut out: Vec<MachineAgents> = states
        .into_iter()
        .filter(|(key, _)| {
            key.strip_prefix(KEY_PREFIX)
                .is_some_and(|m| m != this_machine)
        })
        .filter_map(|(_, value)| serde_json::from_value::<MachineAgents>(value).ok())
        .filter(|m| {
            m.machine != this_machine
                && !(this_machine_id.is_some() && m.machine_id.as_deref() == this_machine_id)
                && !m.is_stale(now)
        })
        .collect();
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

impl SyncClient {
    /// Publish this machine's agents (last write wins; one key per machine).
    pub async fn put_live_agents(&self, snapshot: &MachineAgents) -> Result<(), IdeError> {
        let key = format!("{KEY_PREFIX}{}", snapshot.machine);
        let value = serde_json::to_value(snapshot.clone().capped())?;
        self.put_state(&key, &value, None).await.map(|_| ())
    }

    /// Remove this machine's published list (`machine` is the key suffix), so
    /// no other machine or reader sees it any more. A key that is already
    /// gone is fine.
    pub async fn delete_live_agents(&self, machine: &str) -> Result<(), IdeError> {
        self.delete_state(&format!("{KEY_PREFIX}{machine}")).await
    }

    /// Every machine's snapshot, this one included; filter with [`others`].
    pub async fn list_live_agents(&self) -> Result<Vec<(String, serde_json::Value)>, IdeError> {
        let http = self.http.clone();
        self.with_refresh(move |base_url, token| {
            let http = http.clone();
            async move {
                let url = format!("{base_url}{SYNC_API}/state");
                let resp = http
                    .get(&url)
                    .query(&[("prefix", KEY_PREFIX)])
                    .bearer_auth(&token)
                    .send()
                    .await?;
                let status = resp.status();
                if !status.is_success() {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(IdeError::Http { status, body });
                }
                let parsed: PullAllResponse = resp.json().await?;
                // An older server ignores `prefix` and returns everything.
                Ok(parsed
                    .states
                    .into_iter()
                    .filter(|s| s.key.starts_with(KEY_PREFIX))
                    .map(|s| (s.key, s.value))
                    .collect())
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(machine: &str, age_secs: i64, now: DateTime<Utc>) -> MachineAgents {
        MachineAgents {
            machine: machine.to_string(),
            label: machine.to_uppercase(),
            platform: "win32".to_string(),
            updated_at: now - chrono::Duration::seconds(age_secs),
            agents: vec![],
            machine_id: None,
        }
    }

    fn agent_with_text(query: &str, response: &str, tool: &str) -> LiveAgent {
        LiveAgent {
            id: Some("sid".into()),
            project: "p".into(),
            agent: "claude".into(),
            status: "running".into(),
            query: Some(query.into()),
            response: Some(response.into()),
            tool: Some(tool.into()),
            last_activity: None,
            remote_url: None,
            background_tasks: 0,
        }
    }

    #[test]
    fn redaction_catches_common_secrets() {
        let cases = [
            "export AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
            "key AKIAIOSFODNN7EXAMPLE in use",
            "curl -H 'Authorization: Bearer abcdef1234567890abcdef'", // gitleaks:allow (fake, redaction test)
            "token ghp_0123456789abcdefghijABCDEFGHIJ012345", // gitleaks:allow (fake, redaction test)
            "use sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123",
            "OPENAI sk-proj-ABCDEFGHIJKLMNOPQRSTUV123",
            "password=hunter2hunter2",
            r#"{"client_secret": "s3cr3tvalue"}"#,
            "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----", // gitleaks:allow (fake, redaction test)
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEA (cut off",
            "postgres://admin:pa55word@db.example.com/x",
            "xoxb-1234567890-abcdefghij",
            "jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U", // gitleaks:allow (fake, redaction test)
        ];
        for text in cases {
            let out = redact_secrets(text);
            assert!(out.contains(REDACTED), "not redacted: {text} -> {out}");
        }
        assert!(!redact_secrets("password=hunter2hunter2").contains("hunter2"));
        assert!(!redact_secrets("AKIAIOSFODNN7EXAMPLE").contains("AKIA"));
        assert!(!redact_secrets("postgres://admin:pa55word@db/x").contains("pa55word"));
    }

    /// Gates review finding 3: each of these went through unredacted.
    #[test]
    fn redaction_catches_the_secrets_the_review_found() {
        let cases = [
            ("DB_PASS=s3cretvalue1", "s3cretvalue1"),
            ("SMTP_PASS: mailpass99", "mailpass99"),
            ("mysql -u root -pHunter2xyz -h db", "Hunter2xyz"),
            ("psql --password Sup3rS3cret -h db", "Sup3rS3cret"),
            ("psql --password=Sup3rS3cret", "Sup3rS3cret"),
            ("Cookie: session=abc123def456ghi", "abc123def456ghi"),
            (
                "Authorization: token 0123456789abcdef0123456789abcdef01234567",
                "0123456789abcdef0123456789abcdef01234567",
            ),
            ("key re_AbCd1234_EfGh5678IjKl9012MnOp", "re_AbCd1234_EfGh5678IjKl9012MnOp"), // gitleaks:allow (fake, redaction test)
            ("npm_abcdefghijklmnopqrstuvwxyz0123456789", "npm_abcdefghijklmnopqrstuvwxyz0123456789"), // gitleaks:allow (fake, redaction test)
            ("hf_abcdefghijklmnopqrstuvwxyzABCDEFGH", "hf_abcdefghijklmnopqrstuvwxyzABCDEFGH"), // gitleaks:allow (fake, redaction test)
            (
                "SG.abcdefghijklmnopqrstuv.ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789", // gitleaks:allow (fake, redaction test)
                "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
            ),
            (
                "post to https://hooks.slack.com/services/T000/B000/XXXXXXXXXXXXXXXX", // gitleaks:allow (fake, redaction test)
                "XXXXXXXXXXXXXXXX",
            ),
            (
                "https://discord.com/api/webhooks/123456789/abcDEF_ghi-JKL", // gitleaks:allow (fake, redaction test)
                "abcDEF_ghi-JKL",
            ),
            (
                "R2 secret 0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0",
                "0f1e2d3c4b5a69788796a5b4c3d2e1f0",
            ),
            (
                r#"PASSWORD="correct horse battery staple""#,
                "horse battery staple",
            ),
        ];
        for (text, secret) in cases {
            let out = redact_secrets(text);
            assert!(out.contains(REDACTED), "not redacted: {text} -> {out}");
            assert!(!out.contains(secret), "leaked {secret}: {text} -> {out}");
        }
        // The flag and header names stay readable.
        assert_eq!(redact_secrets("mysql -u root -pHunter2xyz"), "mysql -u root -p[redacted]");
        assert_eq!(redact_secrets("Cookie: a=b; c=d"), "Cookie: [redacted]");
    }

    #[test]
    fn redaction_leaves_ordinary_text_alone() {
        for text in [
            "Fix the failing test in transcripts.rs",
            "running cargo test -p mnemos-ide-sync",
            "Bash",
            "the token count is 42",
            "2 background tasks",
            "checked out 8f14e45fceea167a5a36dedd4bea2543f9b2c3d1",
            "mkdir -p src/bin",
            "cargo test -p mnemos-ide-sync --lib",
        ] {
            assert_eq!(redact_secrets(text), text);
        }
    }

    #[test]
    fn publish_is_status_only_unless_text_is_shared() {
        let now = Utc::now();
        let mut s = snapshot("pc", 0, now);
        s.agents = vec![agent_with_text("deploy with password=hunter2hunter2", "done", "Bash")];

        let status_only = s.clone().for_publish(false);
        let a = &status_only.agents[0];
        assert_eq!(a.query, None);
        assert_eq!(a.response, None);
        assert_eq!(a.tool.as_deref(), Some("Bash"));
        assert_eq!(a.status, "running");
        assert_eq!(a.id.as_deref(), Some("sid"));
        let json = serde_json::to_value(&status_only).unwrap();
        assert!(json["agents"][0].get("query").is_none());
        assert!(json["agents"][0].get("response").is_none());

        let shared = s.for_publish(true);
        let q = shared.agents[0].query.as_deref().unwrap();
        assert!(q.starts_with("deploy with "));
        assert!(!q.contains("hunter2"), "{q}");
        assert_eq!(shared.agents[0].response.as_deref(), Some("done"));
    }

    #[test]
    fn machine_id_is_optional_both_ways() {
        let now = Utc::now();
        let old = serde_json::json!({
            "machine": "pc",
            "label": "PC",
            "platform": "win32",
            "updated_at": now,
            "agents": [],
        });
        let parsed: MachineAgents = serde_json::from_value(old).unwrap();
        assert_eq!(parsed.machine_id, None);
        assert!(serde_json::to_value(&parsed).unwrap().get("machine_id").is_none());
        let with_id = MachineAgents {
            machine_id: Some("abc123".into()),
            ..parsed
        };
        let json = serde_json::to_value(&with_id).unwrap();
        assert_eq!(json["machine_id"], "abc123");
        // The key scheme stays the host-name suffix.
        assert_eq!(json["machine"], "pc");
    }

    #[test]
    fn machine_keys_are_safe() {
        assert_eq!(machine_key("Alice-MacBook-Pro.local"), "alice-macbook-pro");
        assert_eq!(machine_key("ALICE"), "alice");
        assert_eq!(machine_key("a b/c"), "a-b-c");
        assert_eq!(machine_key("  "), "unknown");
        assert_eq!(machine_key(&"x".repeat(100)).len(), 63);
    }

    #[test]
    fn others_drops_self_stale_and_garbage() {
        let now = Utc::now();
        let states = vec![
            (
                "agents-live:mac".to_string(),
                serde_json::to_value(snapshot("mac", 5, now)).unwrap(),
            ),
            (
                "agents-live:pc".to_string(),
                serde_json::to_value(snapshot("pc", 30, now)).unwrap(),
            ),
            (
                "agents-live:old".to_string(),
                serde_json::to_value(snapshot("old", 600, now)).unwrap(),
            ),
            (
                "agents-live:bad".to_string(),
                serde_json::json!({"nope": 1}),
            ),
            (
                "projects".to_string(),
                serde_json::to_value(snapshot("x", 0, now)).unwrap(),
            ),
        ];
        let got = others(states, "mac", None, now);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].machine, "pc");
    }

    /// The host name changed since the last publish: the old key carries
    /// this machine's id and is dropped too, while another machine (and one
    /// with no id) stays.
    #[test]
    fn others_drops_this_machine_under_an_old_host_name() {
        let now = Utc::now();
        let with_id = |machine: &str, id: Option<&str>| {
            let mut s = snapshot(machine, 5, now);
            s.machine_id = id.map(Into::into);
            serde_json::to_value(s).unwrap()
        };
        let states = vec![
            ("agents-live:mac-new".to_string(), with_id("mac-new", Some("m1"))),
            ("agents-live:mac-old".to_string(), with_id("mac-old", Some("m1"))),
            ("agents-live:pc".to_string(), with_id("pc", Some("p1"))),
            ("agents-live:legacy".to_string(), with_id("legacy", None)),
        ];
        let got: Vec<String> = others(states.clone(), "mac-new", Some("m1"), now)
            .into_iter()
            .map(|m| m.machine)
            .collect();
        assert_eq!(got, vec!["legacy".to_string(), "pc".to_string()]);
        // Without an id of its own, only the key tells this machine apart.
        assert_eq!(others(states, "mac-new", None, now).len(), 3);
    }

    #[test]
    fn capped_bounds_list_and_text() {
        let now = Utc::now();
        let mut s = snapshot("pc", 0, now);
        s.agents = (0..100)
            .map(|_| LiveAgent {
                id: None,
                project: "p".into(),
                agent: "claude".into(),
                status: "idle".into(),
                query: Some("æ".repeat(500)),
                response: None,
                tool: None,
                last_activity: None,
                remote_url: Some(format!(
                    "https://claude.ai/code/session_{}",
                    "x".repeat(300)
                )),
                background_tasks: 0,
            })
            .collect();
        let s = s.capped();
        assert_eq!(s.agents.len(), MAX_AGENTS);
        assert_eq!(
            s.agents[0].query.as_ref().unwrap().chars().count(),
            MAX_TEXT_CHARS
        );
        assert_eq!(s.agents[0].remote_url, None);
    }

    #[test]
    fn live_agent_new_fields_are_optional_both_ways() {
        // What a client from before `remote_url` / `background_tasks` sends.
        let old = serde_json::json!({
            "id": "abc",
            "project": "p",
            "agent": "claude",
            "status": "running",
            "tool": "Bash",
        });
        let parsed: LiveAgent = serde_json::from_value(old).unwrap();
        assert_eq!(parsed.remote_url, None);
        assert_eq!(parsed.background_tasks, 0);
        // Without the new values, nothing new goes on the wire.
        let json = serde_json::to_value(&parsed).unwrap();
        assert!(json.get("remote_url").is_none());
        assert!(json.get("background_tasks").is_none());

        let new = LiveAgent {
            remote_url: Some("https://claude.ai/code/session_01Test".into()),
            background_tasks: 2,
            ..parsed
        };
        let json = serde_json::to_value(&new).unwrap();
        assert_eq!(json["remote_url"], "https://claude.ai/code/session_01Test");
        assert_eq!(json["background_tasks"], 2);
        let back: LiveAgent = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back, new);

        // An older reader (no such fields, unknown ones ignored) still parses it.
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OldLiveAgent {
            id: Option<String>,
            project: String,
            agent: String,
            status: String,
            tool: Option<String>,
        }
        let old: OldLiveAgent = serde_json::from_value(json).unwrap();
        assert_eq!(old.status, "running");
    }
}
