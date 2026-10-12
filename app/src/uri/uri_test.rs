use self::parse_url_paths::{get_item_data_from_mnemos_link, MnemosWebLink};
use super::*;
use crate::launch_configs::launch_config::make_mock_single_window_launch_config;
use crate::ChannelState;

#[test]
fn test_find_matching_config() {
    let mut configs: Vec<LaunchConfig> = vec![];
    for i in 0..5 {
        add_mock_config_with_name(
            (String::from("config") + i.to_string().as_str()).as_str(),
            &mut configs,
        );
    }

    let with_extension = "config1.yaml";
    assert_eq!(
        find_matching_config(with_extension, &configs),
        Some(&configs[1])
    );

    let no_extension = "config4";
    assert_eq!(
        find_matching_config(no_extension, &configs),
        Some(&configs[4])
    );

    let caps_insensitive = "ConFig3";
    assert_eq!(
        find_matching_config(caps_insensitive, &configs),
        Some(&configs[3])
    );

    let missing_config = "missing";
    assert_eq!(find_matching_config(missing_config, &configs), None);
}

#[test]
fn test_find_matching_config_with_spaces() {
    let mut configs: Vec<LaunchConfig> = vec![];
    for i in 0..3 {
        add_mock_config_with_name(
            (String::from("config") + i.to_string().as_str()).as_str(),
            &mut configs,
        );
    }

    let with_space = "config 3.yaml";
    add_mock_config_with_name(with_space, &mut configs);
    assert_eq!(
        find_matching_config(with_space, &configs),
        Some(&configs[3])
    );

    let more_space = " a ";
    add_mock_config_with_name(more_space, &mut configs);
    assert_eq!(
        find_matching_config(more_space, &configs),
        Some(&configs[4])
    );
}

#[test]
fn test_find_matching_configs_special_chars() {
    let mut configs: Vec<LaunchConfig> = vec![];
    for i in 0..3 {
        add_mock_config_with_name(
            (String::from("config") + i.to_string().as_str()).as_str(),
            &mut configs,
        );
    }

    // test special characters
    let special_ascii = "yes! this_works,too-even[braces}and(parens'.";
    add_mock_config_with_name(special_ascii, &mut configs);
    assert_eq!(
        find_matching_config(special_ascii, &configs),
        Some(&configs[3])
    );

    // test emojis
    let bread = "🍞";
    add_mock_config_with_name(bread, &mut configs);
    assert_eq!(find_matching_config(bread, &configs), Some(&configs[4]));
}

fn add_mock_config_with_name(name: &str, configs: &mut Vec<LaunchConfig>) {
    let mut new_config = make_mock_single_window_launch_config();
    new_config.name = name.to_string();
    new_config.windows[0].tabs[0].title = Some(String::from("First tab from config ") + name);
    configs.push(new_config);
}

#[test]
fn test_get_launch_config_path() {
    assert_eq!(
        get_launch_config_path("/path/to/a/config"),
        Some(String::from("path/to/a/config")),
    );
    assert_eq!(
        get_launch_config_path("/hello%20world.yaml"),
        Some(String::from("hello world.yaml")),
    );
    assert_eq!(
        get_launch_config_path("/%3Bhello%20%23world!"),
        Some(String::from(";hello #world!")),
    );
    assert_eq!(
        get_launch_config_path("/yes%21%20this_works%2Ctoo-even%5Bbraces%7Dand%28parens%27."),
        Some(String::from("yes! this_works,too-even[braces}and(parens'."))
    );
    assert_eq!(
        get_launch_config_path("/%F0%9F%8D%9E"),
        Some(String::from("🍞"))
    );
    assert_eq!(
        get_launch_config_path("/..filename_.with_dots.."),
        Some(String::from("..filename_.with_dots.."))
    );
}

#[test]
fn test_get_launch_config_path_invalid() {
    assert_eq!(get_launch_config_path(""), None);
    assert_eq!(get_launch_config_path("/"), None);
    assert_eq!(get_launch_config_path("%2F"), None);
    assert_eq!(get_launch_config_path("/../outside"), None);
    assert_eq!(get_launch_config_path("/..%2Foutside"), None);
    assert_eq!(get_launch_config_path("/A/.."), None);
    assert_eq!(get_launch_config_path("/A/../B"), None);
    assert_eq!(get_launch_config_path("//absolute"), None);
    assert_eq!(get_launch_config_path("/%2Fabsolute sneaky"), None);
    assert_eq!(get_launch_config_path("//../very_bad/.."), None);
}

#[test]
fn test_remove_extension() {
    assert_eq!(remove_extension(""), None);
    assert_eq!(remove_extension(".yaml"), Some(""));
    assert_eq!(remove_extension(" .yaml"), Some(" "));
    assert_eq!(remove_extension("config.yaml"), Some("config"));
    assert_eq!(remove_extension("..yaml"), Some("."));
    assert_eq!(remove_extension("config"), None);
    assert_eq!(remove_extension("🍞.yaml"), Some("🍞"));
}

#[test]
fn test_mnemos_web_link_notebook() {
    assert_eq!(
        get_item_data_from_mnemos_link(
            &Url::parse(&format!(
                "{}/drive/notebook/Performance-Analysis-LkDlnAe34vfYD2JXsAkssc?focused_folder_id=test_uid00000000000123&invitee_email=test@example.com",
                ChannelState::server_root_url()
            ))
            .unwrap()
        ),
        Some(MnemosWebLink::DriveObject(Box::new(OpenMnemosDriveObjectArgs {
            object_type: ObjectType::Notebook,
server_id: ServerId::from_string_lossy("LkDlnAe34vfYD2JXsAkssc"),
            settings: OpenMnemosDriveObjectSettings {
                focused_folder_id: Some(ServerId::from(123)),
                invitee_email: Some(String::from("test@example.com")),
            },
        })))
    );
}

#[test]
fn test_mnemos_web_link_session() {
    assert_eq!(
        get_item_data_from_mnemos_link(
            &Url::parse(&format!(
                "{}/session/317d0686-7a0b-4b67-806b-aaa3e9df501b?
                pwd=6f727249-af9f-4025-a240-59df40a4c64b",
                ChannelState::server_root_url()
            ))
            .unwrap()
        ),
        Some(MnemosWebLink::Session)
    );
}

#[test]
fn test_mnemos_web_link_workflow() {
    assert_eq!(
        get_item_data_from_mnemos_link(
            &Url::parse(&format!(
                "{}/drive/workflow/Remove-all-stopped-docker-container-image-and-volumes-ZCJSkai2gpwTqpBFs5HOfZ",
                ChannelState::server_root_url()
            ))
            .unwrap()
        ),
        Some(MnemosWebLink::DriveObject(Box::new(OpenMnemosDriveObjectArgs {
            object_type: ObjectType::Workflow,
server_id: ServerId::from_string_lossy("ZCJSkai2gpwTqpBFs5HOfZ"),
            settings: OpenMnemosDriveObjectSettings::default(),
        })))
    );
}

#[test]
fn test_mnemos_web_link_failure() {
    assert_eq!(
        get_item_data_from_mnemos_link(&Url::parse("https://google.com").unwrap()),
        None
    );
}

#[test]
fn test_action_cloud_agent_setup_parse() {
    let url = Url::parse(&format!(
        "{}://action/cloud_agent_setup",
        ChannelState::url_scheme()
    ))
    .unwrap();

    let action = Action::parse(&url).unwrap();
    assert!(matches!(action, Action::CloudAgentSetup));
}

#[test]
fn test_action_new_cloud_agent_conversation_parse() {
    let url = Url::parse(&format!(
        "{}://action/new_cloud_agent_conversation",
        ChannelState::url_scheme()
    ))
    .unwrap();

    let action = Action::parse(&url).unwrap();
    assert!(matches!(action, Action::NewCloudAgentConversation));
}

#[test]
fn test_action_new_agent_conversation_parse() {
    let url = Url::parse(&format!(
        "{}://action/new_agent_conversation",
        ChannelState::url_scheme()
    ))
    .unwrap();

    let action = Action::parse(&url).unwrap();
    assert!(matches!(action, Action::NewAgentConversation));
}

// -- handle_incoming_uri redaction -------------------------------------------
//
// These tests cover the fix for GH #737: the entry log inside
// `handle_incoming_uri` used to write the full URL (including the Firebase
// `refresh_token` query parameter) to `mnemos.log` at `info` level before any
// redaction ran. They validate the redaction helper and the error messages
// produced by `validate_custom_uri` to ensure that the fallback `warn`
// emitted on invalid URIs never embeds the query string either.

/// The redacted log representation must contain scheme/host/path for triage
/// but must never contain the query string or any token material.
#[test]
fn safe_url_log_fields_redacts_refresh_token() {
    let url = Url::parse(&format!(
        "{}://auth/desktop_redirect?refresh_token=SENSITIVE_TOKEN&state=abc&user_uid=u",
        ChannelState::url_scheme()
    ))
    .unwrap();

    let logged = safe_url_log_fields(&url);

    assert!(
        logged.contains(&format!("scheme={}", ChannelState::url_scheme())),
        "expected scheme in redacted log, got: {logged}"
    );
    assert!(
        logged.contains("host=auth"),
        "expected host in redacted log, got: {logged}"
    );
    assert!(
        logged.contains("path=/desktop_redirect"),
        "expected path in redacted log, got: {logged}"
    );
    assert!(
        !logged.contains("refresh_token"),
        "redacted log must not contain refresh_token: {logged}"
    );
    assert!(
        !logged.contains("SENSITIVE_TOKEN"),
        "redacted log must not contain the token value: {logged}"
    );
    assert!(
        !logged.contains("state="),
        "redacted log must not contain state query param: {logged}"
    );
    assert!(
        !logged.contains("user_uid"),
        "redacted log must not contain user_uid: {logged}"
    );
}

/// The redacted log representation must drop generic OAuth query parameters
/// (`code=`, `access_token=`, `custom_token=`, `token=`) regardless of host.
#[test]
fn safe_url_log_fields_redacts_generic_oauth_params() {
    let url = Url::parse(&format!(
        "{}://mcp/oauth_callback?code=AUTH_CODE&state=xyz&access_token=AT&custom_token=CT&token=RAW",
        ChannelState::url_scheme()
    ))
    .unwrap();

    let logged = safe_url_log_fields(&url);

    for forbidden in [
        "code=",
        "AUTH_CODE",
        "access_token",
        "AT",
        "custom_token",
        "CT",
        "token=RAW",
        "state=",
    ] {
        assert!(
            !logged.contains(forbidden),
            "redacted log must not contain {forbidden:?}: {logged}"
        );
    }
    assert!(logged.contains("host=mcp"), "expected host: {logged}");
    assert!(
        logged.contains("path=/oauth_callback"),
        "expected path: {logged}"
    );
}

/// Drive links carry user-identifiable `invitee_email` values in the query.
/// The entry log must not surface them on non-dogfood channels.
#[test]
fn safe_url_log_fields_redacts_invitee_email() {
    let url = Url::parse(&format!(
        "{}://drive/notebook?id=abc&invitee_email=alice@example.com",
        ChannelState::url_scheme()
    ))
    .unwrap();

    let logged = safe_url_log_fields(&url);

    assert!(
        !logged.contains("alice@example.com"),
        "redacted log must not contain invitee email: {logged}"
    );
    assert!(
        !logged.contains("invitee_email"),
        "redacted log must not contain invitee_email key: {logged}"
    );
    assert!(logged.contains("host=drive"), "expected host: {logged}");
}

/// URL fragments are not currently used as secret carriers by Mnemos today, but
/// the entry log's contract is "scheme + host + path only", so fragments must
/// be dropped as well.
#[test]
fn safe_url_log_fields_drops_fragment() {
    let url = Url::parse(&format!(
        "{}://auth/desktop_redirect#sensitive_fragment",
        ChannelState::url_scheme()
    ))
    .unwrap();

    let logged = safe_url_log_fields(&url);

    assert!(
        !logged.contains("sensitive_fragment"),
        "redacted log must not contain url fragment: {logged}"
    );
    assert!(
        !logged.contains('#'),
        "redacted log must not contain any fragment separator: {logged}"
    );
}

/// `file://` URLs route through the same entry log. `file://` URLs on macOS
/// have no host; the helper must not panic and must report `host=-` so the
/// format string stays well-formed.
#[test]
fn safe_url_log_fields_handles_file_urls_without_host() {
    let url = Url::parse("file:///tmp/foo.md").unwrap();

    let logged = safe_url_log_fields(&url);

    assert!(logged.contains("scheme=file"), "expected scheme: {logged}");
    assert!(
        logged.contains("host=-"),
        "expected host placeholder: {logged}"
    );
    assert!(
        logged.contains("path=/tmp/foo.md"),
        "expected path: {logged}"
    );
}

/// `validate_custom_uri` returns `anyhow::Error`s whose messages feed the
/// non-dogfood `log::warn!("Custom URI is invalid: {e:?}")` fallback in
/// `handle_incoming_uri`. Those messages must never embed the full URL, its
/// query string, or its fragment — otherwise the fallback warn line becomes
/// a second secret leak.
#[test]
fn validate_custom_uri_errors_do_not_leak_query_string() {
    // Unexpected scheme.
    let url = Url::parse("https://auth/desktop_redirect?refresh_token=LEAKED").unwrap();
    let err = validate_custom_uri(&url).unwrap_err();
    let msg = format!("{err:?}");
    assert!(!msg.contains("refresh_token"), "{msg}");
    assert!(!msg.contains("LEAKED"), "{msg}");

    // Unexpected host.
    let url = Url::parse(&format!(
        "{}://unknown_host/desktop_redirect?refresh_token=LEAKED",
        ChannelState::url_scheme()
    ))
    .unwrap();
    let err = validate_custom_uri(&url).unwrap_err();
    let msg = format!("{err:?}");
    assert!(!msg.contains("refresh_token"), "{msg}");
    assert!(!msg.contains("LEAKED"), "{msg}");

    // Unexpected path for a host that doesn't allow arbitrary paths.
    let url = Url::parse(&format!(
        "{}://auth/not_the_redirect?refresh_token=LEAKED",
        ChannelState::url_scheme()
    ))
    .unwrap();
    let err = validate_custom_uri(&url).unwrap_err();
    let msg = format!("{err:?}");
    assert!(!msg.contains("refresh_token"), "{msg}");
    assert!(!msg.contains("LEAKED"), "{msg}");
}

#[test]
fn test_parse_tab_path_expands_tilde() {
    let url = Url::parse("mnemos://action/new_tab?path=~/Projects").unwrap();
    let home = dirs::home_dir().expect("HOME must be set for this test");
    assert_eq!(parse_tab_path(&url), Some(home.join("Projects")));
}

#[test]
fn test_parse_tab_path_expands_url_encoded_tilde() {
    // `%7E` and `%2F` are URL-encoded `~` and `/`.
    let url = Url::parse("mnemos://action/new_tab?path=%7E%2FProjects").unwrap();
    let home = dirs::home_dir().expect("HOME must be set for this test");
    assert_eq!(parse_tab_path(&url), Some(home.join("Projects")));
}

#[test]
fn test_parse_tab_path_absolute_path_unchanged() {
    let url = Url::parse("mnemos://action/new_tab?path=/tmp/foo").unwrap();
    assert_eq!(parse_tab_path(&url), Some(PathBuf::from("/tmp/foo")));
}

#[test]
fn test_parse_tab_path_relative_path_unchanged() {
    let url = Url::parse("mnemos://action/new_tab?path=relative/dir").unwrap();
    assert_eq!(parse_tab_path(&url), Some(PathBuf::from("relative/dir")));
}

#[test]
fn test_parse_tab_path_missing_returns_none() {
    let url = Url::parse("mnemos://action/new_tab").unwrap();
    assert_eq!(parse_tab_path(&url), None);
}

#[test]
fn test_parse_tab_path_bare_tilde() {
    let url = Url::parse("mnemos://action/new_tab?path=~").unwrap();
    let home = dirs::home_dir().expect("HOME must be set for this test");
    assert_eq!(parse_tab_path(&url), Some(home));
}

/// Mnemos IDE: any web page can open these, so they ask before acting.
#[test]
fn links_that_open_a_shell_or_run_a_launch_config_need_confirmation() {
    let scheme = ChannelState::url_scheme();
    for (link, asks) in [
        (format!("{scheme}://action/new_tab?path=~/Projects"), true),
        (format!("{scheme}://action/new_window?path=/tmp"), true),
        (format!("{scheme}://launch/work.yaml"), true),
        (format!("{scheme}://action/new_agent_conversation"), false),
        (format!("{scheme}://agents/focus?id=abc"), false),
        (format!("{scheme}://settings/appearance"), false),
    ] {
        let url = Url::parse(&link).unwrap();
        let host = validate_custom_uri(&url).unwrap();
        assert_eq!(confirmation_prompt(&host, &url).is_some(), asks, "{link}");
    }
}

/// Mnemos IDE: the docker, linear, codex, create_environment and
/// focus_cloud_mode handlers are gone.
#[test]
fn removed_link_handlers_are_rejected() {
    let scheme = ChannelState::url_scheme();
    for link in [
        format!("{scheme}://codex"),
        format!("{scheme}://linear/work?prompt=hi"),
    ] {
        assert!(validate_custom_uri(&Url::parse(&link).unwrap()).is_err(), "{link}");
    }
    for path in [
        "docker/open_subshell?container_id=x",
        "create_environment?repo=a",
        "focus_cloud_mode",
    ] {
        let url = Url::parse(&format!("{scheme}://action/{path}")).unwrap();
        assert!(Action::parse(&url).is_err(), "{path}");
    }
}

/// Mnemos IDE: link paths stay under home, and UNC paths never get through.
#[test]
fn link_paths_must_stay_under_home() {
    let home = tempfile::tempdir().unwrap();
    let inside = home.path().join("code");
    std::fs::create_dir(&inside).unwrap();
    let outside = tempfile::tempdir().unwrap();

    assert_eq!(
        validate_link_path(&inside, home.path()).unwrap(),
        dunce::canonicalize(&inside).unwrap()
    );
    assert!(validate_link_path(outside.path(), home.path()).is_err());
    assert!(validate_link_path(&inside.join("../.."), home.path()).is_err());
    assert!(validate_link_path(Path::new("relative/dir"), home.path()).is_err());
    assert!(validate_link_path(Path::new("//server/share"), home.path()).is_err());
    assert!(validate_link_path(Path::new("\\\\server\\share"), home.path()).is_err());
    assert!(validate_link_path(&home.path().join("missing"), home.path()).is_err());
    #[cfg(unix)]
    {
        let link = home.path().join("escape");
        std::os::unix::fs::symlink(outside.path(), &link).unwrap();
        assert!(validate_link_path(&link, home.path()).is_err());
    }
}

/// Mnemos IDE: what a second launch forwards, what the Finder Services and
/// the Explorer verbs send, all open at once, anywhere on disk.
#[test]
fn local_launch_links_open_without_asking_outside_home() {
    let outside = tempfile::tempdir().unwrap();
    for new_window in [false, true] {
        let url = local_launch_url(new_window, outside.path()).unwrap();
        assert_eq!(url.scheme(), local_launch_scheme());
        assert_ne!(url.scheme(), ChannelState::url_scheme());
        // Not a mnemos:// link, so it never reaches the confirmation step.
        assert!(validate_custom_uri(&url).is_err());
        let action = Action::parse(&url).unwrap();
        assert!(matches!(
            (new_window, action),
            (false, Action::NewTab) | (true, Action::NewWindow)
        ));
        assert_eq!(parse_tab_path(&url).as_deref(), Some(outside.path()));
        assert_eq!(
            validate_local_launch_path(outside.path()).unwrap(),
            dunce::canonicalize(outside.path()).unwrap()
        );
    }
    // Paths with query metacharacters survive the round trip.
    let odd = outside.path().join("a&b#c d");
    std::fs::create_dir(&odd).unwrap();
    let url = local_launch_url(false, &odd).unwrap();
    assert_eq!(parse_tab_path(&url), Some(odd));
    assert!(validate_local_launch_path(Path::new("relative/dir")).is_err());
}

#[test]
fn explorer_verb_flags_become_local_launch_links() {
    let dir = tempfile::tempdir().unwrap();
    let args = mnemos_cli::AppArgs {
        new_tab: Some(dir.path().to_path_buf()),
        new_window: Some(dir.path().to_path_buf()),
        ..Default::default()
    };
    let urls = local_launch_urls(&args);
    assert_eq!(urls.len(), 2);
    assert!(urls.iter().all(|url| url.scheme() == local_launch_scheme()));
    assert!(matches!(Action::parse(&urls[0]), Ok(Action::NewTab)));
    assert!(matches!(Action::parse(&urls[1]), Ok(Action::NewWindow)));
    assert!(local_launch_urls(&mnemos_cli::AppArgs::default()).is_empty());

    // `"%V"` on a drive root arrives as `C:"`.
    assert_eq!(explorer_path(Path::new("C:\"")), PathBuf::from("C:\\"));
    assert_eq!(explorer_path(Path::new("D:\\code\"")), PathBuf::from("D:\\code"));
    assert_eq!(explorer_path(Path::new("/srv/x")), PathBuf::from("/srv/x"));
}

/// Mnemos IDE: only the MCP and Appearance settings pages are linkable.
#[test]
fn only_mcp_and_appearance_settings_links_open() {
    let scheme = ChannelState::url_scheme();
    let parse = |path: &str| {
        parse_settings_link(&Url::parse(&format!("{scheme}://settings/{path}")).unwrap())
    };
    assert_eq!(parse("appearance"), Some(SettingsLink::Appearance));
    assert_eq!(
        parse("mcp?autoinstall=GitHub"),
        Some(SettingsLink::Mcp {
            autoinstall: Some("GitHub".to_string())
        })
    );
    assert_eq!(parse("mcp"), Some(SettingsLink::Mcp { autoinstall: None }));
    for removed in [
        "teams?invite=a@example.com",
        "billing_and_usage",
        "environments",
        "platform",
    ] {
        assert_eq!(parse(removed), None, "{removed}");
    }
}
