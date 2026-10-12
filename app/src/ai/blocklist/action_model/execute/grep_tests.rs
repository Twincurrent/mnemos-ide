use super::*;
use crate::terminal::{model::secrets::regexes::FIREBASE_AUTH_DOMAIN, shell::ShellType};

/// Clears the process-wide secret regexes when the test ends, pass or fail.
struct ClearSecretRegexes;

impl Drop for ClearSecretRegexes {
    fn drop(&mut self) {
        crate::terminal::model::set_user_and_enterprise_secret_regexes(
            std::iter::empty(),
            std::iter::empty(),
        );
    }
}

// Mnemos IDE: the secret regexes are global. Serial with the other tests that set or read them,
// and cleared afterwards, so a secret_redaction test that expects none no longer sees this one.
#[test]
#[serial_test::serial]
fn test_create_redacted_grep_error_event() {
    let _clear = ClearSecretRegexes;
    crate::terminal::model::set_user_and_enterprise_secret_regexes(
        [&regex::Regex::new(FIREBASE_AUTH_DOMAIN).expect("Should be able to construct regex")],
        std::iter::empty(), // No enterprise secrets
    );

    // Create input with a known secret pattern (Firebase domain)
    let queries = vec![
        "normal query".to_string(),
        "query with mnemos-server-staging.firebaseapp.com secret".to_string(),
    ];
    let path = "path/to/file/with/mnemos-server-staging.firebaseapp.com/secret".to_string();
    let shell_type = Some(ShellType::Bash);
    let working_directory = Some("/users/test/mnemos-server-staging.firebaseapp.com".to_string());
    let absolute_path =
        "/absolute/path/with/mnemos-server-staging.firebaseapp.com/secret".to_string();
    let error = GrepError::new("Error message".to_string())
        .with_command("grep mnemos-server-staging.firebaseapp.com".to_string())
        .with_output("Output with mnemos-server-staging.firebaseapp.com".to_string());

    // Call the function with the test inputs
    let event = create_redacted_grep_error_event(
        true,
        None,
        queries.clone(),
        path.clone(),
        shell_type,
        working_directory.clone(),
        absolute_path.clone(),
        error,
    );

    // Verify the telemetry event has redacted secrets
    if let TelemetryEvent::GrepToolFailed {
        queries: Some(redacted_queries),
        path: Some(redacted_path),
        shell_type: _,
        working_directory: Some(redacted_working_directory),
        absolute_path: Some(redacted_absolute_path),
        command: Some(redacted_command),
        output: Some(redacted_output),
        error: _,
        server_output_id: _,
    } = event
    {
        // Verify secrets are redacted from all relevant fields
        assert_eq!(redacted_queries.len(), 2);
        assert_eq!(redacted_queries[0], "normal query");
        assert!(!redacted_queries[1].contains("mnemos-server-staging.firebaseapp.com"));
        assert!(redacted_queries[1].contains("*****"));

        assert!(!redacted_path.contains("mnemos-server-staging.firebaseapp.com"));
        assert!(redacted_path.contains("*****"));

        assert!(!redacted_working_directory.contains("mnemos-server-staging.firebaseapp.com"));
        assert!(redacted_working_directory.contains("*****"));

        assert!(!redacted_absolute_path.contains("mnemos-server-staging.firebaseapp.com"));
        assert!(redacted_absolute_path.contains("*****"));

        assert!(!redacted_command.contains("mnemos-server-staging.firebaseapp.com"));
        assert!(redacted_command.contains("*****"));

        assert!(!redacted_output.contains("mnemos-server-staging.firebaseapp.com"));
        assert!(redacted_output.contains("*****"));
    } else {
        panic!("Expected GrepToolFailed event");
    }
}
