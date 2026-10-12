use super::derive_http_origin_from_ws_url;
use super::tagged_logfile_name;

#[test]
fn wss_becomes_https_and_strips_path() {
    let got = derive_http_origin_from_ws_url("wss://rtc.app.web.test/graphql/v2");
    assert_eq!(got.as_deref(), Some("https://rtc.app.web.test"));
}

#[test]
fn ws_becomes_http_and_preserves_port() {
    let got = derive_http_origin_from_ws_url("ws://localhost:8080/graphql/v2");
    assert_eq!(got.as_deref(), Some("http://localhost:8080"));
}

#[test]
fn unparseable_input_returns_none() {
    assert!(derive_http_origin_from_ws_url("not a url").is_none());
    assert!(derive_http_origin_from_ws_url("https://app.web.test").is_none());
}

#[test]
fn tagged_logfile_name_inserts_a_profile_before_the_log_extension() {
    assert_eq!(
        tagged_logfile_name("mnemos.log", Some("devtest")),
        "mnemos-devtest.log"
    );
    assert_eq!(
        tagged_logfile_name("mnemos.log", Some("dev")),
        "mnemos-dev.log"
    );
}

#[test]
fn tagged_logfile_name_uses_the_debug_tag_without_a_profile() {
    assert_eq!(
        tagged_logfile_name("mnemos.log", Some("debug")),
        "mnemos-debug.log"
    );
}

#[test]
fn tagged_logfile_name_appends_to_a_name_without_log_extension() {
    assert_eq!(tagged_logfile_name("mnemos", Some("dev")), "mnemos-dev");
    assert_eq!(
        tagged_logfile_name("mnemos.txt", Some("dev")),
        "mnemos.txt-dev"
    );
}

#[test]
fn tagged_logfile_name_sanitizes_path_characters_in_the_tag() {
    assert_eq!(
        tagged_logfile_name("mnemos.log", Some("a/b")),
        "mnemos-a_b.log"
    );
    assert_eq!(
        tagged_logfile_name("mnemos.log", Some("../x\\y z")),
        "mnemos-___x_y_z.log"
    );
    assert_eq!(
        tagged_logfile_name("mnemos.log", Some("Dev_1-a")),
        "mnemos-Dev_1-a.log"
    );
}

#[test]
fn tagged_logfile_name_without_a_tag_returns_the_base_unchanged() {
    assert_eq!(tagged_logfile_name("mnemos.log", None), "mnemos.log");
    assert_eq!(tagged_logfile_name("mnemos.log", Some("")), "mnemos.log");
    assert_eq!(tagged_logfile_name("", Some("dev")), "");
    assert_eq!(tagged_logfile_name("", None), "");
}
