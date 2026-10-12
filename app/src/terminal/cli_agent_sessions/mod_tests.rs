use mnemos_ui::{App, EntityId};

use super::event::{parse_event, CLIAgentEvent, CLIAgentEventPayload, CLIAgentEventType};
use super::{
    CLIAgentInputEntrypoint, CLIAgentInputState, CLIAgentSession, CLIAgentSessionContext,
    CLIAgentSessionStatus, CLIAgentSessionsModel,
};
use crate::ai::blocklist::{InputConfig, InputType};
use crate::terminal::CLIAgent;

/// A local Claude session with no listener, the shape command detection and
/// file ingest produce.
fn claude_session(status: CLIAgentSessionStatus) -> CLIAgentSession {
    CLIAgentSession {
        agent: CLIAgent::Claude,
        status,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        draft_text: None,
        remote_host: None,
        custom_command_prefix: None,
    }
}

fn claude_event(event: CLIAgentEventType, payload: CLIAgentEventPayload) -> CLIAgentEvent {
    CLIAgentEvent {
        v: 1,
        agent: CLIAgent::Claude,
        event,
        session_id: Some("abc".to_owned()),
        cwd: Some("/tmp/proj".to_owned()),
        project: Some("proj".to_owned()),
        payload,
    }
}

#[test]
fn parse_stop_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"write a haiku","response":"Memory is safe","transcript_path":"/tmp/t.jsonl"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();

    assert_eq!(notif.v, 1);
    assert_eq!(notif.agent, CLIAgent::Claude);
    assert_eq!(notif.event, CLIAgentEventType::Stop);
    assert_eq!(notif.session_id.as_deref(), Some("abc"));
    assert_eq!(notif.cwd.as_deref(), Some("/tmp/proj"));
    assert_eq!(notif.project.as_deref(), Some("proj"));
    assert_eq!(notif.payload.query.as_deref(), Some("write a haiku"));
    assert_eq!(notif.payload.response.as_deref(), Some("Memory is safe"));
    assert_eq!(
        notif.payload.transcript_path.as_deref(),
        Some("/tmp/t.jsonl")
    );
}

#[test]
fn legacy_warp_sentinel_parses_same_as_native() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"write a haiku","response":"Memory is safe","transcript_path":"/tmp/t.jsonl"}"#;
    let native = parse_event(Some("mnemos://cli-agent"), body).unwrap();
    let legacy = parse_event(Some("warp://cli-agent"), body).unwrap();

    assert_eq!(native.v, legacy.v);
    assert_eq!(native.agent, legacy.agent);
    assert_eq!(native.event, legacy.event);
    assert_eq!(native.session_id, legacy.session_id);
    assert_eq!(native.cwd, legacy.cwd);
    assert_eq!(native.project, legacy.project);
    assert_eq!(native.payload.query, legacy.payload.query);
    assert_eq!(native.payload.response, legacy.payload.response);
    assert_eq!(
        native.payload.transcript_path,
        legacy.payload.transcript_path
    );
}

#[test]
fn cli_agent_session_context_title_like_text_uses_trimmed_summary() {
    let context = CLIAgentSessionContext {
        summary: Some("  Reviewing changes  ".to_string()),
        query: Some("Latest prompt".to_string()),
        ..Default::default()
    };

    assert_eq!(
        context.title_like_text(),
        Some("Reviewing changes".to_string())
    );
}

#[test]
fn cli_agent_session_context_latest_user_prompt_uses_trimmed_query() {
    let context = CLIAgentSessionContext {
        summary: Some("Reviewing changes".to_string()),
        query: Some("  Latest prompt  ".to_string()),
        ..Default::default()
    };

    assert_eq!(
        context.latest_user_prompt(),
        Some("Latest prompt".to_string())
    );
}

#[test]
fn cli_agent_session_context_title_helpers_ignore_empty_text() {
    let context = CLIAgentSessionContext {
        summary: Some("  ".to_string()),
        query: Some("".to_string()),
        ..Default::default()
    };

    assert_eq!(context.title_like_text(), None);
    assert_eq!(context.latest_user_prompt(), None);
}

#[test]
fn parse_permission_request_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"permission_request","session_id":"abc","cwd":"/tmp/proj","project":"proj","summary":"Wants to run Bash: rm -rf /tmp","tool_name":"Bash","tool_input":{"command":"rm -rf /tmp"}}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::PermissionRequest);
    assert_eq!(
        notif.payload.summary.as_deref(),
        Some("Wants to run Bash: rm -rf /tmp")
    );
    assert_eq!(notif.payload.tool_name.as_deref(), Some("Bash"));
    assert_eq!(
        notif.payload.tool_input_preview.as_deref(),
        Some("rm -rf /tmp")
    );
}

#[test]
fn parse_permission_request_with_file_path() {
    let body = r#"{"v":1,"agent":"claude","event":"permission_request","session_id":"abc","cwd":"/tmp","project":"tmp","tool_name":"Write","tool_input":{"file_path":"/tmp/test.py","content":"print('hi')"}}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();

    assert_eq!(
        notif.payload.tool_input_preview.as_deref(),
        Some("/tmp/test.py")
    );
}

#[test]
fn parse_idle_prompt_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"idle_prompt","session_id":"abc","cwd":"/tmp","project":"tmp","summary":"Claude is waiting for your input"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::IdlePrompt);
    assert_eq!(
        notif.payload.summary.as_deref(),
        Some("Claude is waiting for your input")
    );
}

#[test]
fn parse_session_start_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"session_start","session_id":"abc","cwd":"/tmp","project":"tmp","plugin_version":"1.1.0"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::SessionStart);
    assert_eq!(notif.payload.plugin_version.as_deref(), Some("1.1.0"));
}

#[test]
fn returns_none_for_wrong_sentinel() {
    let body = r#"{"v":1,"agent":"claude","event":"stop"}"#;
    assert!(parse_event(Some("Claude Code"), body).is_none());
}

#[test]
fn returns_none_for_missing_title() {
    let body = r#"{"v":1,"agent":"claude","event":"stop"}"#;
    assert!(parse_event(None, body).is_none());
}

#[test]
fn returns_none_for_invalid_json() {
    assert!(parse_event(Some("mnemos://cli-agent"), "not json").is_none());
}

#[test]
fn handles_unknown_event_type() {
    let body = r#"{"v":1,"agent":"claude","event":"some_future_event"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();
    assert_eq!(
        notif.event,
        CLIAgentEventType::Unknown("some_future_event".to_string())
    );
}

#[test]
fn handles_missing_optional_fields() {
    let body = r#"{"event":"stop"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();

    assert_eq!(notif.v, 1);
    assert_eq!(notif.agent, CLIAgent::Unknown);
    assert_eq!(notif.event, CLIAgentEventType::Stop);
    assert!(notif.session_id.is_none());
    assert!(notif.cwd.is_none());
    assert!(notif.project.is_none());
    assert!(notif.payload.query.is_none());
}

#[test]
fn handles_special_characters_in_values() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","query":"what does \"hello\" mean?","response":"It means greeting. Use: printf(\"hello\")"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();

    assert_eq!(
        notif.payload.query.as_deref(),
        Some("what does \"hello\" mean?")
    );
    assert_eq!(
        notif.payload.response.as_deref(),
        Some("It means greeting. Use: printf(\"hello\")")
    );
}

#[test]
fn rejects_unsupported_schema_version() {
    let body = r#"{"v":2,"agent":"claude","event":"stop"}"#;
    assert!(parse_event(Some("mnemos://cli-agent"), body).is_none());
}

#[test]
fn defaults_to_v1_when_version_missing() {
    let body = r#"{"agent":"claude","event":"stop","query":"hi"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();
    assert_eq!(notif.v, 1);
    assert_eq!(notif.payload.query.as_deref(), Some("hi"));
}

#[test]
fn explicit_v1_parses_correctly() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","query":"test"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();
    assert_eq!(notif.v, 1);
    assert_eq!(notif.payload.query.as_deref(), Some("test"));
}

#[test]
fn parse_prompt_submit_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"prompt_submit","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"fix the bug"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::PromptSubmit);
    assert_eq!(notif.payload.query.as_deref(), Some("fix the bug"));
}

#[test]
fn parse_tool_complete_notification() {
    let body = r#"{"v":1,"agent":"claude","event":"tool_complete","session_id":"abc","cwd":"/tmp/proj","project":"proj","tool_name":"Bash"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();

    assert_eq!(notif.event, CLIAgentEventType::ToolComplete);
    assert_eq!(notif.payload.tool_name.as_deref(), Some("Bash"));
}

#[test]
fn parse_auggie_stop_notification() {
    // Mirrors what the community auggie-mnemos plugin emits on the Stop hook.
    let body = r#"{"v":1,"agent":"auggie","event":"stop","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"write a haiku","response":"Memory is safe"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();

    assert_eq!(notif.agent, CLIAgent::Auggie);
    assert_eq!(notif.event, CLIAgentEventType::Stop);
    assert_eq!(notif.payload.query.as_deref(), Some("write a haiku"));
    assert_eq!(notif.payload.response.as_deref(), Some("Memory is safe"));
}

#[test]
fn apply_event_preserves_input_session() {
    let input_state = CLIAgentInputState::Open {
        entrypoint: CLIAgentInputEntrypoint::CtrlG,
        previous_input_config: InputConfig {
            input_type: InputType::Shell,
            is_locked: false,
        },
        previous_was_lock_set_with_empty_buffer: true,
    };
    let mut session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state,
        should_auto_toggle_input: false,
        listener: None,
        remote_host: None,
        plugin_version: None,
        draft_text: None,
        custom_command_prefix: None,
    };

    let event = CLIAgentEvent {
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::PermissionRequest,
        session_id: Some("abc".to_string()),
        cwd: Some("/tmp/proj".to_string()),
        project: Some("proj".to_string()),
        payload: CLIAgentEventPayload {
            summary: Some("Needs approval".to_string()),
            ..Default::default()
        },
    };

    session.apply_event(&event);

    assert_eq!(session.input_state, input_state);
}

#[test]
fn is_remote_returns_true_when_remote_host_is_set() {
    let session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        draft_text: None,
        remote_host: Some("user@devbox".to_owned()),
        custom_command_prefix: None,
    };
    assert!(session.is_remote());
}

#[test]
fn is_remote_returns_false_when_remote_host_is_none() {
    let session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        remote_host: None,
        plugin_version: None,
        draft_text: None,
        custom_command_prefix: None,
    };
    assert!(!session.is_remote());
}

#[test]
fn local_failure_is_shared_across_local_sessions() {
    let mut model = CLIAgentSessionsModel::new();

    model.record_plugin_auto_failure(CLIAgent::Claude, None);

    assert!(model.has_plugin_auto_failed(CLIAgent::Claude, &None));
}

#[test]
fn local_failure_does_not_affect_remote_host() {
    let mut model = CLIAgentSessionsModel::new();

    model.record_plugin_auto_failure(CLIAgent::Claude, None);

    let remote = Some("user@devbox".to_owned());
    assert!(!model.has_plugin_auto_failed(CLIAgent::Claude, &remote));
}

#[test]
fn remote_failure_does_not_affect_local() {
    let mut model = CLIAgentSessionsModel::new();

    model.record_plugin_auto_failure(CLIAgent::Claude, Some("user@devbox".to_owned()));

    assert!(!model.has_plugin_auto_failed(CLIAgent::Claude, &None));
}

#[test]
fn remote_failures_are_independent_per_host() {
    let mut model = CLIAgentSessionsModel::new();

    let host_a = Some("user@host-a".to_owned());
    let host_b = Some("user@host-b".to_owned());

    model.record_plugin_auto_failure(CLIAgent::Claude, host_a.clone());

    assert!(model.has_plugin_auto_failed(CLIAgent::Claude, &host_a));
    assert!(!model.has_plugin_auto_failed(CLIAgent::Claude, &host_b));
}

#[test]
fn failure_tracking_is_independent_per_agent() {
    let mut model = CLIAgentSessionsModel::new();

    model.record_plugin_auto_failure(CLIAgent::Claude, None);

    assert!(model.has_plugin_auto_failed(CLIAgent::Claude, &None));
    assert!(!model.has_plugin_auto_failed(CLIAgent::Gemini, &None));
}

#[test]
fn session_start_sets_plugin_version() {
    let mut session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        draft_text: None,
        remote_host: None,
        custom_command_prefix: None,
    };

    let event = CLIAgentEvent {
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::SessionStart,
        session_id: Some("abc".to_owned()),
        cwd: Some("/tmp".to_owned()),
        project: Some("proj".to_owned()),
        payload: CLIAgentEventPayload {
            plugin_version: Some("1.5.0".to_owned()),
            ..Default::default()
        },
    };

    session.apply_event(&event);
    assert_eq!(session.plugin_version.as_deref(), Some("1.5.0"));
}

#[test]
fn session_start_without_plugin_version_leaves_none() {
    let mut session = CLIAgentSession {
        agent: CLIAgent::Claude,
        status: CLIAgentSessionStatus::InProgress,
        session_context: CLIAgentSessionContext::default(),
        input_state: CLIAgentInputState::Closed,
        should_auto_toggle_input: false,
        listener: None,
        plugin_version: None,
        draft_text: None,
        remote_host: None,
        custom_command_prefix: None,
    };

    let event = CLIAgentEvent {
        v: 1,
        agent: CLIAgent::Claude,
        event: CLIAgentEventType::SessionStart,
        session_id: Some("abc".to_owned()),
        cwd: None,
        project: None,
        payload: CLIAgentEventPayload::default(),
    };

    session.apply_event(&event);
    assert_eq!(session.plugin_version, None);
}

#[test]
fn ingested_event_is_dropped_once_osc_has_delivered_for_the_terminal() {
    App::test((), |mut app| async move {
        let sessions = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let terminal = EntityId::new();

        sessions.update(&mut app, |model, ctx| {
            model.set_session(
                terminal,
                claude_session(CLIAgentSessionStatus::InProgress),
                ctx,
            );
            // OSC path: this marks the terminal as one whose OSC transport works.
            model.update_from_event(
                terminal,
                &claude_event(
                    CLIAgentEventType::PromptSubmit,
                    CLIAgentEventPayload {
                        query: Some("fix the bug".to_owned()),
                        ..Default::default()
                    },
                ),
                ctx,
            );
            // The file copy of an event must not be applied a second time.
            model.apply_ingested_event(
                terminal,
                &claude_event(CLIAgentEventType::Stop, CLIAgentEventPayload::default()),
                ctx,
            );
        });

        sessions.read(&app, |model, _| {
            let session = model.session(terminal).expect("session should exist");
            assert_eq!(session.status, CLIAgentSessionStatus::InProgress);
            assert_eq!(
                session.session_context.query.as_deref(),
                Some("fix the bug")
            );
        });
    })
}

#[test]
fn ingested_event_is_applied_when_osc_never_delivered() {
    // Windows: ConPTY drops OSC 777, so the file is the only transport.
    App::test((), |mut app| async move {
        let sessions = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let terminal = EntityId::new();

        sessions.update(&mut app, |model, ctx| {
            model.set_session(
                terminal,
                claude_session(CLIAgentSessionStatus::InProgress),
                ctx,
            );
            model.apply_ingested_event(
                terminal,
                &claude_event(CLIAgentEventType::Stop, CLIAgentEventPayload::default()),
                ctx,
            );
        });

        sessions.read(&app, |model, _| {
            let session = model.session(terminal).expect("session should exist");
            assert_eq!(session.status, CLIAgentSessionStatus::Success);
        });
    })
}

#[test]
fn removing_the_session_forgets_osc_delivery() {
    App::test((), |mut app| async move {
        let sessions = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let terminal = EntityId::new();

        sessions.update(&mut app, |model, ctx| {
            model.set_session(
                terminal,
                claude_session(CLIAgentSessionStatus::InProgress),
                ctx,
            );
            model.update_from_event(
                terminal,
                &claude_event(
                    CLIAgentEventType::PromptSubmit,
                    CLIAgentEventPayload::default(),
                ),
                ctx,
            );
            model.remove_session(terminal, ctx);
            // A new session in the same terminal starts without OSC delivery,
            // so file events apply to it again.
            model.set_session(
                terminal,
                claude_session(CLIAgentSessionStatus::InProgress),
                ctx,
            );
            model.apply_ingested_event(
                terminal,
                &claude_event(CLIAgentEventType::Stop, CLIAgentEventPayload::default()),
                ctx,
            );
        });

        sessions.read(&app, |model, _| {
            let session = model.session(terminal).expect("session should exist");
            assert_eq!(session.status, CLIAgentSessionStatus::Success);
        });
    })
}

#[test]
fn ingested_non_session_start_event_creates_no_session() {
    // A late file line (e.g. the Stop that lands after the claude block
    // completed and the view removed the session) must not create a zombie.
    App::test((), |mut app| async move {
        let sessions = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let terminal = EntityId::new();

        sessions.update(&mut app, |model, ctx| {
            for event_type in [CLIAgentEventType::PromptSubmit, CLIAgentEventType::Stop] {
                model.apply_ingested_event(
                    terminal,
                    &claude_event(event_type, CLIAgentEventPayload::default()),
                    ctx,
                );
            }
        });

        sessions.read(&app, |model, _| {
            assert!(model.session(terminal).is_none());
        });
    })
}

#[test]
fn ingested_session_start_creates_a_session() {
    App::test((), |mut app| async move {
        // Creating a session reads AISettings; preferences are in memory
        // under cfg(test).
        app.add_singleton_model(|_| settings::manager::SettingsManager::default());
        app.update(crate::settings::init_and_register_user_preferences);
        app.update(crate::settings::AISettings::register_and_subscribe_to_events);
        let sessions = app.add_singleton_model(|_| CLIAgentSessionsModel::new());
        let terminal = EntityId::new();

        sessions.update(&mut app, |model, ctx| {
            model.apply_ingested_event(
                terminal,
                &claude_event(
                    CLIAgentEventType::SessionStart,
                    CLIAgentEventPayload {
                        plugin_version: Some("2.2.0".to_owned()),
                        ..Default::default()
                    },
                ),
                ctx,
            );
        });

        sessions.read(&app, |model, _| {
            let session = model
                .session(terminal)
                .expect("an ingested SessionStart should create a session");
            assert_eq!(session.agent, CLIAgent::Claude);
            assert_eq!(session.status, CLIAgentSessionStatus::InProgress);
            assert_eq!(session.session_context.cwd.as_deref(), Some("/tmp/proj"));
            assert_eq!(session.plugin_version.as_deref(), Some("2.2.0"));
            assert!(session.listener.is_none());
        });
    })
}

#[test]
fn parse_stop_failure_notification() {
    // Shape sent by the plugin's StopFailure hook; error_type is ignored.
    let body = r#"{"v":1,"agent":"claude","event":"stop_failure","session_id":"abc","cwd":"/tmp/proj","project":"proj","query":"fix the bug","response":"API Error: Connection lost while your computer was asleep","error_type":"server_error","transcript_path":"/tmp/t.jsonl"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();

    assert_eq!(notif.agent, CLIAgent::Claude);
    assert_eq!(notif.event, CLIAgentEventType::StopFailure);
    assert_eq!(notif.payload.query.as_deref(), Some("fix the bug"));
    assert_eq!(
        notif.payload.response.as_deref(),
        Some("API Error: Connection lost while your computer was asleep")
    );
}

#[test]
fn stop_failure_blocks_with_the_error_until_the_next_prompt() {
    let mut session = claude_session(CLIAgentSessionStatus::Success);

    let prompt = claude_event(
        CLIAgentEventType::PromptSubmit,
        CLIAgentEventPayload {
            query: Some("fix the bug".to_owned()),
            ..Default::default()
        },
    );
    assert_eq!(
        session.apply_event(&prompt),
        Some(CLIAgentSessionStatus::InProgress)
    );

    let failure = claude_event(
        CLIAgentEventType::StopFailure,
        CLIAgentEventPayload {
            query: Some("fix the bug".to_owned()),
            response: Some("API Error: Connection lost while your computer was asleep".to_owned()),
            ..Default::default()
        },
    );
    let blocked = CLIAgentSessionStatus::Blocked {
        message: Some("API Error: Connection lost while your computer was asleep".to_owned()),
    };
    assert_eq!(session.apply_event(&failure), Some(blocked.clone()));
    assert_eq!(session.status, blocked);
    assert_eq!(
        session.session_context.query.as_deref(),
        Some("fix the bug")
    );
    assert_eq!(
        session.session_context.response.as_deref(),
        Some("API Error: Connection lost while your computer was asleep")
    );

    let next_prompt = claude_event(
        CLIAgentEventType::PromptSubmit,
        CLIAgentEventPayload {
            query: Some("try again".to_owned()),
            ..Default::default()
        },
    );
    assert_eq!(
        session.apply_event(&next_prompt),
        Some(CLIAgentSessionStatus::InProgress)
    );
    assert_eq!(session.status, CLIAgentSessionStatus::InProgress);
    assert_eq!(session.session_context.query.as_deref(), Some("try again"));
    assert_eq!(session.session_context.response, None);
}

#[test]
fn stop_failure_with_blank_fields_uses_fallback_and_keeps_query() {
    let mut session = claude_session(CLIAgentSessionStatus::InProgress);
    session.session_context.query = Some("fix the bug".to_owned());
    session.session_context.response = Some("stale".to_owned());

    // The plugin sends empty strings when the transcript is missing.
    let failure = claude_event(
        CLIAgentEventType::StopFailure,
        CLIAgentEventPayload {
            query: Some(String::new()),
            response: Some("   ".to_owned()),
            ..Default::default()
        },
    );
    let blocked = CLIAgentSessionStatus::Blocked {
        message: Some("Turn failed (API error)".to_owned()),
    };
    assert_eq!(session.apply_event(&failure), Some(blocked.clone()));
    assert_eq!(session.status, blocked);
    assert_eq!(
        session.session_context.query.as_deref(),
        Some("fix the bug")
    );
    assert_eq!(session.session_context.response, None);
}

#[test]
fn stop_without_new_fields_parses_as_before() {
    let body =
        r#"{"v":1,"agent":"claude","event":"stop","session_id":"abc","query":"q","response":"r"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();
    assert_eq!(notif.payload.remote_url, None);
    assert_eq!(notif.payload.background_tasks, None);

    let mut session = claude_session(CLIAgentSessionStatus::InProgress);
    assert_eq!(
        session.apply_event(&notif),
        Some(CLIAgentSessionStatus::Success)
    );
    assert_eq!(session.session_context.background_tasks, 0);
}

#[test]
fn parse_remote_url_and_background_tasks() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","session_id":"abc","remote_url":"https://claude.ai/code/session_01TestFakeSessionIdAbc","background_tasks":2,"some_future_field":{"x":1}}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();
    assert_eq!(
        notif.payload.remote_url.as_deref(),
        Some("https://claude.ai/code/session_01TestFakeSessionIdAbc")
    );
    assert_eq!(notif.payload.background_tasks, Some(2));
}

#[test]
fn parse_drops_foreign_remote_url_and_bad_count() {
    let body = r#"{"v":1,"agent":"claude","event":"stop","remote_url":"https://evil.example/code/session_01Test","background_tasks":"two"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();
    assert_eq!(notif.event, CLIAgentEventType::Stop);
    assert_eq!(notif.payload.remote_url, None);
    assert_eq!(notif.payload.background_tasks, None);

    let body = r#"{"v":1,"agent":"claude","event":"stop","background_tasks":-1}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();
    assert_eq!(notif.payload.background_tasks, None);
}

#[test]
fn stop_with_background_tasks_stays_in_progress_until_they_finish() {
    let mut session = claude_session(CLIAgentSessionStatus::InProgress);
    let stop = |n: u32| {
        claude_event(
            CLIAgentEventType::Stop,
            CLIAgentEventPayload {
                query: Some("fix it".to_owned()),
                response: Some("Started 2 agents.".to_owned()),
                background_tasks: Some(n),
                ..Default::default()
            },
        )
    };

    assert_eq!(
        session.apply_event(&stop(2)),
        Some(CLIAgentSessionStatus::InProgress)
    );
    assert_eq!(session.session_context.background_tasks, 2);
    assert_eq!(
        session.session_context.response.as_deref(),
        Some("Started 2 agents.")
    );

    // One reports back; Claude Code runs a turn for it and stops again.
    assert_eq!(
        session.apply_event(&stop(1)),
        Some(CLIAgentSessionStatus::InProgress)
    );
    assert_eq!(session.session_context.background_tasks, 1);

    assert_eq!(
        session.apply_event(&stop(0)),
        Some(CLIAgentSessionStatus::Success)
    );
    assert_eq!(session.session_context.background_tasks, 0);
}

#[test]
fn prompt_submit_clears_background_count() {
    let mut session = claude_session(CLIAgentSessionStatus::InProgress);
    session.session_context.background_tasks = 3;
    session.apply_event(&claude_event(
        CLIAgentEventType::PromptSubmit,
        CLIAgentEventPayload {
            query: Some("next".to_owned()),
            ..Default::default()
        },
    ));
    assert_eq!(session.session_context.background_tasks, 0);
}

#[test]
fn remote_url_sticks_until_replaced() {
    let mut session = claude_session(CLIAgentSessionStatus::InProgress);
    let first = "https://claude.ai/code/session_01First".to_owned();
    session.apply_event(&claude_event(
        CLIAgentEventType::PromptSubmit,
        CLIAgentEventPayload {
            remote_url: Some(first.clone()),
            ..Default::default()
        },
    ));
    assert_eq!(session.session_context.remote_url.as_ref(), Some(&first));

    // An event without one keeps it.
    session.apply_event(&claude_event(
        CLIAgentEventType::Stop,
        CLIAgentEventPayload::default(),
    ));
    assert_eq!(session.session_context.remote_url.as_ref(), Some(&first));

    let second = "https://claude.ai/code/session_01Second".to_owned();
    session.apply_event(&claude_event(
        CLIAgentEventType::Stop,
        CLIAgentEventPayload {
            remote_url: Some(second.clone()),
            ..Default::default()
        },
    ));
    assert_eq!(session.session_context.remote_url.as_ref(), Some(&second));
}

/// Regression: any output in a pane (`cat` of a file, a remote host, curl)
/// could print `ESC ] 777 ; notify ; mnemos://cli-agent ; {...}` and create
/// phantom agent sessions. Only titles with this PTY's token are trusted.
#[test]
fn cli_agent_titles_need_the_pty_token() {
    use super::event::{check_cli_agent_title, CliAgentTitleCheck::*};
    let token = Some("3f1c9a4e-7b1d-4a8e-9c55-0d2b6f1e8a77");

    assert_eq!(
        check_cli_agent_title(Some("mnemos://cli-agent#3f1c9a4e-7b1d-4a8e-9c55-0d2b6f1e8a77"), token),
        Authentic
    );
    assert_eq!(
        check_cli_agent_title(Some("warp://cli-agent#3f1c9a4e-7b1d-4a8e-9c55-0d2b6f1e8a77"), token),
        Authentic
    );
    // The bare sentinel (Gemini, OpenCode, older plugins) is only
    // unverified: the model trusts it while the pane runs that agent.
    assert_eq!(check_cli_agent_title(Some("mnemos://cli-agent"), token), Unverified);
    assert_eq!(check_cli_agent_title(Some("warp://cli-agent"), token), Unverified);
    // A guess and an empty token are refused.
    assert_eq!(check_cli_agent_title(Some("mnemos://cli-agent#guess"), token), Forged);
    assert_eq!(check_cli_agent_title(Some("mnemos://cli-agent#"), Some("")), Forged);
    // A PTY without a token (remote, shared-session viewer) trusts no token
    // that is not a live terminal's of this app.
    assert_eq!(
        check_cli_agent_title(Some("mnemos://cli-agent#3f1c9a4e-7b1d-4a8e-9c55-0d2b6f1e8a77"), None),
        Forged
    );
    // Ordinary notifications are untouched.
    assert_eq!(check_cli_agent_title(Some("Build done"), token), NotSentinel);
    assert_eq!(check_cli_agent_title(None, token), NotSentinel);
    assert_eq!(check_cli_agent_title(Some("Build #12 done"), token), NotSentinel);
}

/// An agent in a tmux session started from another pane carries that pane's
/// token; it is trusted while that terminal lives, and not after.
#[test]
fn cli_agent_titles_with_another_live_terminals_token_are_trusted() {
    use super::event::{
        check_cli_agent_title, register_live_token, unregister_live_token, CliAgentTitleCheck::*,
    };
    let other = "6b0e2f4a-1c3d-4e5f-8a9b-0c1d2e3f4a5b";
    let title = format!("mnemos://cli-agent#{other}");
    let mine = Some("3f1c9a4e-7b1d-4a8e-9c55-0d2b6f1e8a77");
    assert_eq!(check_cli_agent_title(Some(&title), mine), Forged);
    register_live_token(other);
    assert_eq!(check_cli_agent_title(Some(&title), mine), Authentic);
    unregister_live_token(other);
    assert_eq!(check_cli_agent_title(Some(&title), mine), Forged);
}

/// A bare event is trusted only from the agent the pane is running.
#[test]
fn untokened_events_need_the_pane_to_run_that_agent() {
    use super::event::agent_runs_command;
    use crate::terminal::CLIAgent;
    assert!(agent_runs_command(CLIAgent::Gemini, "gemini --yolo"));
    assert!(agent_runs_command(CLIAgent::OpenCode, "FOO=1 /usr/local/bin/opencode"));
    assert!(agent_runs_command(CLIAgent::Claude, "C:\\tools\\claude.exe -c"));
    assert!(!agent_runs_command(CLIAgent::Gemini, "cat events.txt"));
    assert!(!agent_runs_command(CLIAgent::Claude, "ssh host claude"));
    assert!(!agent_runs_command(CLIAgent::Claude, "tmux attach"));
    assert!(!agent_runs_command(CLIAgent::Unknown, "unknown"));
    assert!(!agent_runs_command(CLIAgent::Gemini, ""));
}

#[test]
fn harness_text_is_not_parsed_as_a_query() {
    let body = r#"{"v":1,"agent":"claude","event":"prompt_submit","query":"<task-notification>\n<task-id>b2v8xxnlx</task-id>\n<status>completed</status>"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();
    assert!(notif.payload.query.is_none());

    let body = r#"{"v":1,"agent":"claude","event":"stop","query":"  <system-reminder>x</system-reminder>"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();
    assert!(notif.payload.query.is_none());

    let body = r#"{"v":1,"agent":"claude","event":"stop","query":"why does <task-notification> show up?"}"#;
    let notif = parse_event(Some("mnemos://cli-agent"), body).unwrap();
    assert_eq!(
        notif.payload.query.as_deref(),
        Some("why does <task-notification> show up?")
    );
}

#[test]
fn turn_without_a_query_keeps_the_last_prompt() {
    let mut session = claude_session(CLIAgentSessionStatus::InProgress);
    session.apply_event(&claude_event(
        CLIAgentEventType::PromptSubmit,
        CLIAgentEventPayload {
            query: Some("fix the bug".to_owned()),
            ..Default::default()
        },
    ));
    // A background task reporting back starts a turn whose harness text was filtered out.
    session.apply_event(&claude_event(
        CLIAgentEventType::PromptSubmit,
        CLIAgentEventPayload::default(),
    ));
    session.apply_event(&claude_event(
        CLIAgentEventType::Stop,
        CLIAgentEventPayload {
            query: Some(String::new()),
            response: Some("done".to_owned()),
            ..Default::default()
        },
    ));
    assert_eq!(session.session_context.query.as_deref(), Some("fix the bug"));
    assert_eq!(session.session_context.response.as_deref(), Some("done"));
}

/// Mnemos IDE: events without a token are off unless the user opts in.
#[test]
fn untokened_cli_agent_events_are_off_by_default() {
    use super::event::accept_untokened_events;
    use settings::Setting as _;
    assert!(!accept_untokened_events());
    let settings = super::event_settings::AcceptUntokenedCliAgentEvents::default_value();
    assert!(!settings);
}

/// Mnemos IDE: only Claude Code's plugin (mnemos-agents) can count as predating the
/// token; other agents' bare events stay behind the opt-in.
#[test]
fn only_claude_plugin_can_predate_the_token() {
    use super::event::installed_plugin_predates_token;
    use crate::terminal::CLIAgent;
    for agent in [
        CLIAgent::Gemini,
        CLIAgent::Codex,
        CLIAgent::OpenCode,
        CLIAgent::Unknown,
    ] {
        assert!(!installed_plugin_predates_token(agent));
    }
}
