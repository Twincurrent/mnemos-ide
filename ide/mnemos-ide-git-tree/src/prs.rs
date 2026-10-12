// SPDX-License-Identifier: AGPL-3.0-only
//! Open pull requests for the PULL REQUESTS section of the tree.
//!
//! Pure parsing and decisions only. The app runs
//! `gh pr list --json <PR_LIST_FIELDS> -R <owner/repo>` off the UI thread and
//! hands stdout to [`parse_pr_list`]; nothing here spawns a process.
//!
//! `-R` is not optional. In a fork that keeps an `upstream` remote (this repo
//! does), bare `gh` resolves to the upstream repo and would list its PRs, so
//! the slug always comes from `origin` via [`origin_github_slug`].

use std::path::Path;

use git2::Repository;
use serde::Deserialize;

use crate::model::{RefKind, RefNode};

/// The `--json` field list the app passes to `gh pr list`. Kept next to the
/// parser so the two cannot drift.
pub const PR_LIST_FIELDS: &str = "number,title,headRefName,isDraft,reviewDecision,\
statusCheckRollup,url,author,isCrossRepository,updatedAt";

/// Review state as GitHub reports it. Only the two decisive states get a chip;
/// "review required" is the default for every open PR and would be noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    None,
}

impl ReviewState {
    pub fn label(self) -> Option<&'static str> {
        match self {
            ReviewState::Approved => Some("approved"),
            ReviewState::ChangesRequested => Some("changes"),
            ReviewState::None => None,
        }
    }
}

/// CI rollup over every check run and commit status on the head commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksState {
    Pass,
    Fail,
    Pending,
    /// No checks reported at all: no chip.
    None,
}

impl ChecksState {
    pub fn label(self) -> Option<&'static str> {
        match self {
            ChecksState::Pass => Some("ci pass"),
            ChecksState::Fail => Some("ci fail"),
            ChecksState::Pending => Some("ci pending"),
            ChecksState::None => None,
        }
    }
}

/// Labels for chips that are not derived from a state enum.
pub const DRAFT_LABEL: &str = "draft";
pub const FORK_LABEL: &str = "fork";

/// One open PR, reduced to what a single tree row shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    pub head_ref: String,
    pub url: String,
    /// `None` when gh omits the author (a deleted account).
    pub author: Option<String>,
    pub is_draft: bool,
    /// Head branch lives in a fork, so it is not on `origin`.
    pub is_cross_repository: bool,
    pub review: ReviewState,
    pub checks: ChecksState,
    /// ISO 8601, as gh prints it. Used for ordering only.
    pub updated_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    head_ref_name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    author: Option<GhAuthor>,
    #[serde(default)]
    is_draft: bool,
    #[serde(default)]
    is_cross_repository: bool,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    status_check_rollup: Option<Vec<GhCheck>>,
    #[serde(default)]
    updated_at: String,
}

#[derive(Deserialize)]
struct GhAuthor {
    #[serde(default)]
    login: String,
}

/// Either a `CheckRun` (`status` + `conclusion`) or a `StatusContext`
/// (`state`). gh tags them with `__typename`, but the fields alone tell them
/// apart, so both shapes go through one struct.
#[derive(Deserialize)]
struct GhCheck {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    conclusion: Option<String>,
    #[serde(default)]
    state: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CheckOutcome {
    Pass,
    Fail,
    Pending,
}

impl GhCheck {
    fn outcome(&self) -> CheckOutcome {
        let upper = |s: &Option<String>| s.as_deref().unwrap_or("").to_ascii_uppercase();

        // StatusContext: a single `state`.
        if self.state.is_some() {
            return match upper(&self.state).as_str() {
                "SUCCESS" => CheckOutcome::Pass,
                "FAILURE" | "ERROR" => CheckOutcome::Fail,
                _ => CheckOutcome::Pending,
            };
        }

        // CheckRun: not finished until `status` is COMPLETED.
        if upper(&self.status) != "COMPLETED" {
            return CheckOutcome::Pending;
        }
        match upper(&self.conclusion).as_str() {
            "FAILURE" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => {
                CheckOutcome::Fail
            }
            // SUCCESS, NEUTRAL, SKIPPED, STALE: none of them block a merge.
            _ => CheckOutcome::Pass,
        }
    }
}

fn summarise_checks(checks: &[GhCheck]) -> ChecksState {
    if checks.is_empty() {
        return ChecksState::None;
    }
    let outcomes: Vec<CheckOutcome> = checks.iter().map(GhCheck::outcome).collect();
    if outcomes.contains(&CheckOutcome::Fail) {
        ChecksState::Fail
    } else if outcomes.contains(&CheckOutcome::Pending) {
        ChecksState::Pending
    } else {
        ChecksState::Pass
    }
}

fn review_state(decision: Option<&str>) -> ReviewState {
    match decision.unwrap_or("") {
        "APPROVED" => ReviewState::Approved,
        "CHANGES_REQUESTED" => ReviewState::ChangesRequested,
        _ => ReviewState::None,
    }
}

/// Parse `gh pr list --json PR_LIST_FIELDS` output. Most recently updated
/// first, which is the order you want to act on them in.
pub fn parse_pr_list(json: &str) -> Result<Vec<PrSummary>, serde_json::Error> {
    let raw: Vec<GhPr> = serde_json::from_str(json.trim())?;
    let mut prs: Vec<PrSummary> = raw
        .into_iter()
        .map(|pr| PrSummary {
            number: pr.number,
            title: pr.title,
            head_ref: pr.head_ref_name,
            url: pr.url,
            author: pr.author.map(|a| a.login).filter(|l| !l.is_empty()),
            is_draft: pr.is_draft,
            is_cross_repository: pr.is_cross_repository,
            review: review_state(pr.review_decision.as_deref()),
            checks: summarise_checks(pr.status_check_rollup.as_deref().unwrap_or(&[])),
            updated_at: pr.updated_at,
        })
        .collect();
    prs.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.number.cmp(&a.number))
    });
    Ok(prs)
}

fn is_slug_segment(s: &str) -> bool {
    !s.is_empty()
        && s != "."
        && s != ".."
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// `owner/repo` for a github.com remote URL, or `None` for anything else.
///
/// Accepts `https://github.com/o/r(.git)`, `git@github.com:o/r(.git)` and
/// `ssh://git@github.com(:port)/o/r(.git)`, with or without userinfo and a
/// trailing slash. GitHub Enterprise and other hosts return `None`, so the
/// section is simply not shown for them.
pub fn github_slug_from_remote_url(url: &str) -> Option<String> {
    let url = url.trim();
    let path = if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("ssh://"))
    {
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let host = host.split_once(':').map_or(host, |(h, _)| h);
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    } else if let Some((user_host, path)) = url.split_once(':') {
        // scp-like `git@github.com:o/r`. A Windows path (`C:\...`) or any
        // other scheme fails the host check below.
        if user_host.contains('/') {
            return None;
        }
        let host = user_host.rsplit_once('@').map_or(user_host, |(_, h)| h);
        if !host.eq_ignore_ascii_case("github.com") {
            return None;
        }
        path
    } else {
        return None;
    };

    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    if !is_slug_segment(owner) || !is_slug_segment(repo) {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

/// `owner/repo` of the repo's `origin` remote, when it is on github.com.
/// Blocking git2 work: call it off the UI thread.
pub fn origin_github_slug(repo_path: &Path) -> Option<String> {
    let repo = Repository::open(repo_path).ok()?;
    let remote = repo.find_remote("origin").ok()?;
    github_slug_from_remote_url(remote.url()?)
}

/// True for `https://github.com/<owner>/<repo>/pull/<number>` and nothing
/// else. The URL comes from gh's output, so it is checked before it is handed
/// to the system browser.
pub fn is_github_pr_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://github.com/") else {
        return false;
    };
    let parts: Vec<&str> = rest.split('/').collect();
    matches!(
        parts.as_slice(),
        [owner, repo, "pull", number]
            if is_slug_segment(owner)
                && is_slug_segment(repo)
                && !number.is_empty()
                && number.chars().all(|c| c.is_ascii_digit())
    )
}

/// A branch name that is safe to put in a refspec and on a git command line:
/// valid as `refs/heads/<name>` and not mistakable for an option.
pub fn is_valid_branch_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && git2::Reference::is_valid_name(&format!("refs/heads/{name}"))
}

/// What clicking a PR row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrCheckout {
    /// The head branch is already checked out; select its tip commit.
    Current { tip_oid: String },
    /// Check out this ref through the normal branch path: the local branch,
    /// or `origin/<head>`, which gets a local tracking branch.
    Branch(String),
    /// Not fetched yet: fetch `origin/<head>`, then check it out.
    Fetch(String),
    /// Head branch lives in a fork. Not checked out; offer the web link.
    Fork,
    /// gh reported a head name git would reject. Not checked out.
    InvalidBranch,
}

/// Decide how to check out a PR's head branch given the refs the tree knows.
pub fn pr_checkout_plan(pr: &PrSummary, refs: &[RefNode]) -> PrCheckout {
    if pr.is_cross_repository {
        return PrCheckout::Fork;
    }
    let head = pr.head_ref.as_str();
    if !is_valid_branch_name(head) {
        return PrCheckout::InvalidBranch;
    }
    if let Some(local) = refs
        .iter()
        .find(|r| r.kind == RefKind::LocalBranch && r.name == head)
    {
        return if local.is_head {
            PrCheckout::Current {
                tip_oid: local.tip_oid.clone(),
            }
        } else {
            PrCheckout::Branch(head.to_owned())
        };
    }
    let remote_name = format!("origin/{head}");
    if refs
        .iter()
        .any(|r| r.kind == RefKind::RemoteBranch && r.name == remote_name)
    {
        return PrCheckout::Branch(remote_name);
    }
    PrCheckout::Fetch(head.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic `gh pr list` output covering every chip: draft, approved,
    /// changes requested, cross-repo, checks pass / fail / pending / none,
    /// and CheckRun mixed with StatusContext.
    const FIXTURE: &str = r#"[
      {"number": 11, "title": "Draft, no checks", "headRefName": "feat/draft",
       "isDraft": true, "reviewDecision": "", "statusCheckRollup": [],
       "url": "https://github.com/acme/app/pull/11",
       "author": {"login": "alice", "is_bot": false}, "isCrossRepository": false,
       "updatedAt": "2026-10-01T10:00:00Z"},
      {"number": 12, "title": "Approved, green", "headRefName": "fix/green",
       "isDraft": false, "reviewDecision": "APPROVED",
       "statusCheckRollup": [
         {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SUCCESS", "name": "build"},
         {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SKIPPED", "name": "deploy"},
         {"__typename": "StatusContext", "state": "SUCCESS", "context": "ci/legacy"}
       ],
       "url": "https://github.com/acme/app/pull/12",
       "author": {"login": "bob"}, "isCrossRepository": false,
       "updatedAt": "2026-10-03T10:00:00Z"},
      {"number": 13, "title": "Changes requested, red", "headRefName": "fix/red",
       "isDraft": false, "reviewDecision": "CHANGES_REQUESTED",
       "statusCheckRollup": [
         {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SUCCESS"},
         {"__typename": "StatusContext", "state": "FAILURE"},
         {"__typename": "CheckRun", "status": "IN_PROGRESS", "conclusion": ""}
       ],
       "url": "https://github.com/acme/app/pull/13",
       "author": {"login": "carol"}, "isCrossRepository": false,
       "updatedAt": "2026-10-02T10:00:00Z"},
      {"number": 14, "title": "From a fork, pending", "headRefName": "main",
       "isDraft": false, "reviewDecision": "REVIEW_REQUIRED",
       "statusCheckRollup": [
         {"__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SUCCESS"},
         {"__typename": "StatusContext", "state": "PENDING"}
       ],
       "url": "https://github.com/acme/app/pull/14",
       "author": null, "isCrossRepository": true,
       "updatedAt": "2026-09-30T10:00:00Z"},
      {"number": 15, "title": "Queued run", "headRefName": "chore/queued",
       "isDraft": false, "reviewDecision": null,
       "statusCheckRollup": [{"__typename": "CheckRun", "status": "QUEUED", "conclusion": null}],
       "url": "https://github.com/acme/app/pull/15",
       "isCrossRepository": false, "updatedAt": "2026-09-29T10:00:00Z"}
    ]"#;

    fn by_number(prs: &[PrSummary], n: u64) -> &PrSummary {
        prs.iter().find(|p| p.number == n).expect("pr present")
    }

    #[test]
    fn parses_fixture_states() {
        let prs = parse_pr_list(FIXTURE).expect("parse");
        assert_eq!(prs.len(), 5);
        // Most recently updated first.
        assert_eq!(
            prs.iter().map(|p| p.number).collect::<Vec<_>>(),
            vec![12, 13, 11, 14, 15]
        );

        let draft = by_number(&prs, 11);
        assert!(draft.is_draft);
        assert_eq!(draft.review, ReviewState::None);
        assert_eq!(draft.checks, ChecksState::None);
        assert_eq!(draft.author.as_deref(), Some("alice"));
        assert_eq!(draft.head_ref, "feat/draft");

        let green = by_number(&prs, 12);
        assert_eq!(green.review, ReviewState::Approved);
        assert_eq!(green.checks, ChecksState::Pass, "skipped counts as passing");

        let red = by_number(&prs, 13);
        assert_eq!(red.review, ReviewState::ChangesRequested);
        assert_eq!(
            red.checks,
            ChecksState::Fail,
            "a failure beats a pending run"
        );

        let fork = by_number(&prs, 14);
        assert!(fork.is_cross_repository);
        assert_eq!(
            fork.review,
            ReviewState::None,
            "review required gets no chip"
        );
        assert_eq!(fork.checks, ChecksState::Pending, "pending StatusContext");
        assert_eq!(fork.author, None);

        let queued = by_number(&prs, 15);
        assert_eq!(queued.checks, ChecksState::Pending, "queued CheckRun");
        assert_eq!(queued.review, ReviewState::None);
    }

    #[test]
    fn parses_empty_list_and_rejects_garbage() {
        assert!(parse_pr_list("[]\n").expect("empty").is_empty());
        assert!(parse_pr_list("no pull requests found").is_err());
    }

    /// Synthetic sample in the shape of `gh pr list -R acme/widget --state all` output
    /// (2026-10-05), trimmed to two entries: CheckRuns carry extra fields
    /// (`detailsUrl`, `workflowName`, ...) and bots have no `name`.
    #[test]
    fn parses_real_gh_shape() {
        let json = r#"[{"author":{"id":"U_x","is_bot":false,"login":"bob","name":""},"headRefName":"sweep/2026-09-27","isCrossRepository":false,"isDraft":true,"number":8,"reviewDecision":"","statusCheckRollup":[],"title":"Overnight sweep","updatedAt":"2026-10-02T09:17:18Z","url":"https://github.com/acme/widget/pull/8"},{"author":{"is_bot":true,"login":"app/dependabot"},"headRefName":"dependabot/github_actions/x-1.0","isCrossRepository":false,"isDraft":false,"number":6,"reviewDecision":"","statusCheckRollup":[{"__typename":"CheckRun","completedAt":"2026-05-05T11:11:08Z","conclusion":"FAILURE","detailsUrl":"https://github.com/acme/widget/actions/runs/1/job/2","name":"Check OWNERS approval","startedAt":"2026-05-05T11:10:53Z","status":"COMPLETED","workflowName":"Check Approvals"},{"__typename":"CheckRun","completedAt":"2026-05-05T11:10:52Z","conclusion":"SUCCESS","detailsUrl":"https://github.com/acme/widget/actions/runs/3/job/4","name":"Verify PR base","startedAt":"2026-05-05T11:10:47Z","status":"COMPLETED","workflowName":"Sync PR Checks"}],"title":"chore(deps): bump x","updatedAt":"2026-05-05T11:11:29Z","url":"https://github.com/acme/widget/pull/6"}]"#;
        let prs = parse_pr_list(json).expect("parse");
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].number, 8);
        assert!(prs[0].is_draft);
        assert_eq!(prs[0].checks, ChecksState::None);
        assert_eq!(prs[1].checks, ChecksState::Fail);
        assert_eq!(prs[1].author.as_deref(), Some("app/dependabot"));
        assert!(prs.iter().all(|p| is_github_pr_url(&p.url)));
    }

    /// Feed real gh output through the parser:
    /// `gh pr list --state open --limit 50 --json <PR_LIST_FIELDS> -R o/r > f.json`
    /// then `MNEMOS_PR_JSON=f.json cargo test -p mnemos-ide-git-tree live_gh_output -- --ignored --nocapture`.
    #[test]
    #[ignore = "reads a captured gh output file named by MNEMOS_PR_JSON"]
    fn live_gh_output() {
        let path = std::env::var("MNEMOS_PR_JSON").expect("MNEMOS_PR_JSON");
        let json = std::fs::read_to_string(path).expect("read");
        let prs = parse_pr_list(&json).expect("parse");
        // Optional: the slug `-R` would get for a checkout (`MNEMOS_REPO=<root>`).
        if let Ok(repo) = std::env::var("MNEMOS_REPO") {
            println!("origin slug: {:?}", origin_github_slug(Path::new(&repo)));
        }
        println!("parsed {} PRs", prs.len());
        for pr in &prs {
            println!(
                "#{} {:?} {:?} {}",
                pr.number, pr.review, pr.checks, pr.head_ref
            );
        }
    }

    #[test]
    fn slug_from_remote_urls() {
        let ok = |url: &str| github_slug_from_remote_url(url);
        assert_eq!(
            ok("https://github.com/acme/widget").as_deref(),
            Some("acme/widget")
        );
        assert_eq!(
            ok("https://github.com/acme/widget.git").as_deref(),
            Some("acme/widget")
        );
        assert_eq!(
            ok("https://github.com/acme/widget/").as_deref(),
            Some("acme/widget")
        );
        assert_eq!(
            ok("https://user@github.com/o/r.git").as_deref(),
            Some("o/r")
        );
        assert_eq!(
            ok("git@github.com:acme/widget.git").as_deref(),
            Some("acme/widget")
        );
        assert_eq!(ok("git@github.com:o/r").as_deref(), Some("o/r"));
        assert_eq!(
            ok("ssh://git@github.com/o/my.repo.git").as_deref(),
            Some("o/my.repo")
        );
        assert_eq!(
            ok("ssh://git@github.com:22/o/r.git").as_deref(),
            Some("o/r")
        );
        assert_eq!(ok("https://GitHub.com/o/r").as_deref(), Some("o/r"));

        for bad in [
            "https://gitlab.com/o/r.git",
            "git@gitlab.com:o/r.git",
            "https://github.example.com/o/r",
            "https://github.com.evil.io/o/r",
            "http://github.com/o/r",
            "https://github.com/o",
            "https://github.com/o/r/extra",
            "https://github.com/../r",
            "/srv/git/repo.git",
            "C:\\src\\repo",
            "",
        ] {
            assert_eq!(github_slug_from_remote_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn pr_url_validation() {
        assert!(is_github_pr_url(
            "https://github.com/acme/widget/pull/8"
        ));
        for bad in [
            "http://github.com/o/r/pull/1",
            "https://github.com/o/r/issues/1",
            "https://github.com/o/r/pull/1x",
            "https://github.com/o/r/pull/",
            "https://github.com/o/r/pull/1/files",
            "https://evil.com/https://github.com/o/r/pull/1",
            "https://github.com.evil.io/o/r/pull/1",
            "file:///etc/passwd",
        ] {
            assert!(!is_github_pr_url(bad), "{bad}");
        }
    }

    #[test]
    fn chip_labels_are_ascii() {
        let labels = [
            ReviewState::Approved.label(),
            ReviewState::ChangesRequested.label(),
            ChecksState::Pass.label(),
            ChecksState::Fail.label(),
            ChecksState::Pending.label(),
            Some(DRAFT_LABEL),
            Some(FORK_LABEL),
        ];
        for label in labels.into_iter().flatten() {
            assert!(label.is_ascii(), "{label:?}");
        }
    }

    fn pr(head: &str, cross: bool) -> PrSummary {
        PrSummary {
            number: 1,
            title: "t".into(),
            head_ref: head.into(),
            url: "https://github.com/o/r/pull/1".into(),
            author: None,
            is_draft: false,
            is_cross_repository: cross,
            review: ReviewState::None,
            checks: ChecksState::None,
            updated_at: String::new(),
        }
    }

    fn rnode(name: &str, kind: RefKind, is_head: bool) -> RefNode {
        RefNode {
            name: name.into(),
            kind,
            tip_oid: format!("{name}-tip"),
            is_head,
            ahead: 0,
            behind: 0,
        }
    }

    #[test]
    fn checkout_plan() {
        let refs = vec![
            rnode("main", RefKind::LocalBranch, true),
            rnode("feat/local", RefKind::LocalBranch, false),
            rnode("origin/feat/local", RefKind::RemoteBranch, false),
            rnode("origin/feat/remote", RefKind::RemoteBranch, false),
            rnode("upstream/feat/up", RefKind::RemoteBranch, false),
        ];
        assert_eq!(
            pr_checkout_plan(&pr("main", false), &refs),
            PrCheckout::Current {
                tip_oid: "main-tip".into()
            }
        );
        assert_eq!(
            pr_checkout_plan(&pr("feat/local", false), &refs),
            PrCheckout::Branch("feat/local".into()),
            "an existing local branch wins over origin"
        );
        assert_eq!(
            pr_checkout_plan(&pr("feat/remote", false), &refs),
            PrCheckout::Branch("origin/feat/remote".into())
        );
        assert_eq!(
            pr_checkout_plan(&pr("feat/up", false), &refs),
            PrCheckout::Fetch("feat/up".into()),
            "only origin counts, not other remotes"
        );
        assert_eq!(
            pr_checkout_plan(&pr("feat/local", true), &refs),
            PrCheckout::Fork
        );
        assert_eq!(
            pr_checkout_plan(&pr("--upload-pack=x", false), &refs),
            PrCheckout::InvalidBranch
        );
        assert_eq!(
            pr_checkout_plan(&pr("a..b", false), &refs),
            PrCheckout::InvalidBranch
        );
    }
}
