use rudder_message::Track;
use virtual_fs::VirtualFS;

use super::*;

// Tests that events with UGC are not persisted to desk.
#[test]
fn test_persist_events_doesnt_include_ugc_events() {
    let telemetry_api = TelemetryApi::new();

    VirtualFS::test(
        "test_persist_events_doesnt_include_ugc_events",
        |dirs, _sandbox| {
            // Add one event without UGC
            let user_id = Some("user".into());
            let anonymous_id = "anonymous_id".to_owned();

            mnemos_ui::telemetry::record_event(
                user_id.clone(),
                anonymous_id.clone(),
                "non UGC event name".into(),
                None,  /* payload */
                false, /* contains_ugc  */
                mnemos_ui::time::get_current_time(),
            );

            mnemos_ui::telemetry::record_event(
                user_id.clone(),
                anonymous_id.clone(),
                "UGC event name".into(),
                None, /* payload */
                true, /* contains_ugc  */
                mnemos_ui::time::get_current_time(),
            );

            let file_path = dirs.root().join("rudderstack");

            telemetry_api
                .flush_and_persist_events_at_path(10, PrivacySettingsSnapshot::mock(), &file_path)
                .expect("Should be able to persist events");

            let file_content: Vec<RudderBatchMessage> =
                serde_json::from_reader(File::open(file_path).expect("Failed to open file"))
                    .expect("Failed to parse file");

            // The event queue is process-global, so other tests running in
            // parallel may have persisted events alongside ours. Only assert
            // on the two events this test recorded.
            let event_names: Vec<&str> = file_content
                .iter()
                .map(|message| message.unwrap_track().event.as_str())
                .collect();
            assert!(
                event_names.contains(&"non UGC event name"),
                "non-UGC event should be persisted, got {event_names:?}"
            );
            assert!(
                !event_names.contains(&"UGC event name"),
                "UGC event must not be persisted, got {event_names:?}"
            );
        },
    );
}

// Builds without a telemetry config (the baked stable config has `telemetry_config: null`) have
// no RudderStack destination. Sending a batch must be a no-op returning Ok instead of posting to
// a URL built from an empty root, which fails with "builder error: relative URL without a base".
#[test]
fn test_send_batch_without_telemetry_config_is_noop() {
    // Unit tests run with the default ChannelState, which has no telemetry config. Never
    // override it here: ChannelState is process-global and other tests read it.
    assert!(
        !ChannelState::is_telemetry_available(),
        "precondition: the default test ChannelState must have no telemetry config"
    );

    let batch = vec![RudderBatchMessageWithMetadata {
        message: RudderBatchMessage::Track(Track {
            event: "t18 test event".into(),
            ..Default::default()
        }),
        contains_ugc: false,
    }];

    // The mock snapshot has telemetry enabled and force-enabled, so the privacy gate does not
    // short-circuit and this exercises the telemetry-config gate. Without that gate the empty
    // root URL makes the request builder fail, so Ok here also means no HTTP call was made.
    let result = mnemos_ui::r#async::block_on(
        TelemetryApi::new().send_batch_messages_to_rudder(batch, PrivacySettingsSnapshot::mock()),
    );
    assert!(
        result.is_ok(),
        "sending without a telemetry config should be a no-op, got {result:?}"
    );
}

impl RudderBatchMessage {
    fn unwrap_track(&self) -> &Track {
        match self {
            RudderBatchMessage::Track(track) => track,
            _ => panic!("Expected a track event"),
        }
    }
}
