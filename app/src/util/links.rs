//! External links the app opens. Keep every user-facing URL here, so a fork (or the move to a
//! docs site) changes them in one place, and never point one at a domain, org or package scope
//! nobody owns.

use std::borrow::Cow;

use crate::channel::ChannelState;

/// The public Mnemos source repository: the first non-comment line of `script/source-url`, or
/// `MNEMOS_SOURCE_URL` at build time (see `app/build.rs`). Never hardcode a repository URL in the
/// app: derive it from these constants, or from `env!("MNEMOS_SOURCE_URL")` inside `concat!`.
pub const MNEMOS_REPO_URL: &str = env!("MNEMOS_SOURCE_URL");

/// Where user documentation lives. A placeholder: Mnemos has no docs site yet, so this is the
/// repository (its README and docs). Update it when a docs site exists.
pub const MNEMOS_DOCS_URL: &str = MNEMOS_REPO_URL;

pub const USER_DOCS_URL: &str = MNEMOS_DOCS_URL;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const GITHUB_ISSUES_URL: &str = concat!(env!("MNEMOS_SOURCE_URL"), "/issues");
/// The issue chooser (`/issues/new/choose`).
pub const NEW_ISSUE_URL: &str = concat!(env!("MNEMOS_SOURCE_URL"), "/issues/new/choose");
/// The bug report form, `.github/ISSUE_TEMPLATE/bug.yml`. GitHub prefills its fields from query
/// parameters named after their ids only here, not on the chooser, which drops the query string.
pub const BUG_REPORT_URL: &str =
    concat!(env!("MNEMOS_SOURCE_URL"), "/issues/new?template=bug.yml");
/// Release downloads. Linux packaged installs update from here.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub const RELEASES_URL: &str = concat!(env!("MNEMOS_SOURCE_URL"), "/releases");
/// The default privacy policy: `PRIVACY.md` in the public source repository, which describes what
/// stays local and what sync uploads. A channel config can name another one (a self-hosted
/// server's own policy); open [`privacy_policy_url`], not this constant.
pub const PRIVACY_POLICY_URL: &str = concat!(env!("MNEMOS_SOURCE_URL"), "/blob/main/PRIVACY.md");
/// The default Terms of Service of the sync server. Shared with the onboarding crate through
/// `mnemos_core`; open [`terms_of_service_url`], not this constant.
pub const TERMS_OF_SERVICE_URL: &str = mnemos_core::channel::DEFAULT_TERMS_OF_SERVICE_URL;

/// The privacy policy to link to: the channel config's `server_config.privacy_policy_url` when it
/// sets one, else [`PRIVACY_POLICY_URL`].
pub fn privacy_policy_url() -> Cow<'static, str> {
    ChannelState::privacy_policy_url_override().unwrap_or(Cow::Borrowed(PRIVACY_POLICY_URL))
}

/// The Terms of Service to link to: the channel config's `server_config.terms_of_service_url`
/// when it sets one, else [`TERMS_OF_SERVICE_URL`].
pub fn terms_of_service_url() -> Cow<'static, str> {
    ChannelState::terms_of_service_url()
}

/// The bug report form with the `mnemos-version` and `os-version` fields of `bug.yml` filled in.
/// Keep the parameter names equal to those field ids.
pub fn feedback_form_url() -> String {
    let mut url = url::Url::parse(BUG_REPORT_URL).expect("Should not fail to parse");
    if let Some(version) = ChannelState::app_version() {
        url.query_pairs_mut().append_pair("mnemos-version", version);
    }
    url.query_pairs_mut()
        .append_pair("os-version", &os_info::get().version().to_string());
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_https(link: &str) {
        let url = url::Url::parse(link).unwrap_or_else(|e| panic!("{link:?}: {e}"));
        assert_eq!(url.scheme(), "https", "{link}");
        assert!(url.host_str().is_some_and(|h| !h.is_empty()), "{link}");
    }

    /// Every link here is opened as-is, so each must be a real https URL (upstream left
    /// several blank or empty when the rebrand had no target for them).
    #[test]
    fn every_link_is_a_non_empty_https_url() {
        let links = [
            MNEMOS_REPO_URL,
            MNEMOS_DOCS_URL,
            USER_DOCS_URL,
            GITHUB_ISSUES_URL,
            NEW_ISSUE_URL,
            BUG_REPORT_URL,
            RELEASES_URL,
            PRIVACY_POLICY_URL,
            TERMS_OF_SERVICE_URL,
        ];
        for link in links {
            assert_https(link);
        }
        assert_https(&privacy_policy_url());
        assert_https(&terms_of_service_url());
        assert_https(&feedback_form_url());
    }

    /// The feedback link must open the bug form itself (the chooser drops the query string) and
    /// name the field ids `bug.yml` defines, or GitHub prefills nothing.
    #[test]
    fn feedback_link_prefills_the_bug_form() {
        let url = url::Url::parse(&feedback_form_url()).expect("valid url");
        assert!(url.path().ends_with("/issues/new"), "{url}");
        let pairs: Vec<(String, String)> = url.query_pairs().into_owned().collect();
        assert!(pairs.contains(&("template".to_string(), "bug.yml".to_string())), "{url}");
        assert!(pairs.iter().any(|(k, _)| k == "os-version"), "{url}");

        let template = include_str!("../../../.github/ISSUE_TEMPLATE/bug.yml");
        for (key, _) in pairs.iter().filter(|(k, _)| k != "template") {
            let id_line = format!("id: {key}");
            assert!(
                template.lines().any(|line| line.trim() == id_line),
                "bug.yml has no field with id {key}"
            );
        }
    }

    /// Upstream left many links empty or blank; they opened nothing (or a blank page). Scan the
    /// app and crate sources (tests excluded) for the shapes they took, so a new one fails here.
    /// Point a link at a constant in this module instead, or drop the link.
    #[test]
    fn no_empty_or_blank_links_in_sources() {
        let patterns = [
            r#"open_url\(&?"""#,
            r#"_URL:&(?:'static)?str="";"#,
            r#"hyperlink\([^;]{0,200}?,""(?:\.(?:to_string|to_owned|into)\(\))?,?\)"#,
            r#"link:Some\(""\.(?:to_string|to_owned|into)\(\)"#,
            r#"Some\(""\.(?:to_string|to_owned|into)\(\),?\),None,"#,
            r#"::new\("[^"]*",Some\(""\),?\)"#,
            concat!("about", ":blank"),
        ]
        .map(|p| regex::Regex::new(p).expect("valid pattern"));

        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let roots = [manifest_dir.join("src"), manifest_dir.join("../crates")];
        let mut offenders = Vec::new();
        for root in &roots {
            let walker = walkdir::WalkDir::new(root)
                .into_iter()
                .filter_entry(|entry| {
                    let name = entry.file_name().to_string_lossy();
                    name != "tests" && name != "target"
                });
            for entry in walker.filter_map(Result::ok) {
                let name = entry.file_name().to_string_lossy();
                if !name.ends_with(".rs")
                    || name.ends_with("_test.rs")
                    || name.ends_with("_tests.rs")
                    || name == "tests.rs"
                {
                    continue;
                }
                let Ok(source) = std::fs::read_to_string(entry.path()) else {
                    continue;
                };
                // Drop comment lines, then all whitespace, so rustfmt's line breaks inside a
                // call do not hide one.
                let flat: String = source
                    .lines()
                    .filter(|line| !line.trim_start().starts_with("//"))
                    .flat_map(|line| line.chars())
                    .filter(|c| !c.is_whitespace())
                    .collect();
                for pattern in &patterns {
                    if let Some(found) = pattern.find(&flat) {
                        offenders.push(format!("{}: {}", entry.path().display(), found.as_str()));
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "empty or blank links:\n{}",
            offenders.join("\n")
        );
    }
}
