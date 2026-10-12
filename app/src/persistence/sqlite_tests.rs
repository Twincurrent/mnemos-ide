use std::{path::PathBuf, sync::Arc};

use mnemos_core::features::FeatureFlag;
use mnemos_graphql::scalars::time::ServerTimestamp;

use crate::{
    app_state::{
        AppState, BranchSnapshot, CodePaneSnapShot, CodePaneTabSnapshot, LeafContents, LeafSnapshot,
        PaneFlex, PaneNodeSnapshot, PaneTabSnapshot, SplitDirection, TabSnapshot,
        TerminalPaneSnapshot, WindowSnapshot,
    },
    cloud_object::{CloudObjectPermissions, Owner},
    code::editor_management::CodeSource,
    notebooks::{CloudNotebook, CloudNotebookModel},
    persistence::{model::ObjectPermissions, BlockCompleted, ModelEvent},
    server::ids::ClientId,
    tab::SelectedTabColor,
    terminal::model::block::SerializedBlock,
    terminal::ShellLaunchData,
};

use super::{
    decode_path, deduplicate_events, encode_path, read_sqlite_data, save_app_state, setup_database,
};

#[test]
fn test_deduplicate_snapshots() {
    let local_notebook = CloudNotebook::new_local(
        CloudNotebookModel {
            title: "Hello".to_string(),
            data: "World".to_string(),
            ai_document_id: None,
            conversation_id: None,
        },
        Owner::mock_current_user(),
        None,
        ClientId::new(),
    );
    let completed_block_1 = BlockCompleted {
        pane_id: vec![1, 2, 3],
        block: Arc::new(SerializedBlock::default()),
        is_local: true,
    };
    let completed_block_2 = BlockCompleted {
        pane_id: vec![4, 5, 6],
        block: Arc::new(SerializedBlock::default()),
        is_local: true,
    };
    let snapshot_1 = AppState {
        active_window_index: Some(1),
        block_lists: Default::default(),
        windows: Default::default(),
        running_mcp_servers: Default::default(),
    };
    let snapshot_2 = AppState {
        active_window_index: Some(2),
        block_lists: Default::default(),
        windows: Default::default(),
        running_mcp_servers: Default::default(),
    };
    let snapshot_3 = AppState {
        active_window_index: Some(3),
        block_lists: Default::default(),
        windows: Default::default(),
        running_mcp_servers: Default::default(),
    };

    let original_events = vec![
        ModelEvent::UpsertNotebook {
            notebook: local_notebook.clone(),
        },
        ModelEvent::Snapshot(snapshot_1.clone()),
        ModelEvent::SaveBlock(completed_block_1.clone()),
        ModelEvent::Snapshot(snapshot_2.clone()),
        ModelEvent::SaveBlock(completed_block_2.clone()),
        ModelEvent::Snapshot(snapshot_3.clone()),
        ModelEvent::UpsertNotebook {
            notebook: local_notebook.clone(),
        },
    ];

    let filtered_events = deduplicate_events(original_events);
    assert_eq!(filtered_events.len(), 5);

    assert!(matches!(
        &filtered_events[0],
        &ModelEvent::UpsertNotebook { .. }
    ));
    // The first snapshot should have been filtered out.
    assert!(matches!(&filtered_events[1], &ModelEvent::SaveBlock(_)));
    // The second snapshot should have been filtered out.
    assert!(matches!(&filtered_events[2], &ModelEvent::SaveBlock(_)));
    // The third snapshot should be preserved.
    match &filtered_events[3] {
        ModelEvent::Snapshot(snapshot) => assert_eq!(snapshot, &snapshot_3),
        other => panic!("Expected ModelEvent::Snapshot, got {other:?}"),
    }
    assert!(matches!(
        &filtered_events[4],
        &ModelEvent::UpsertNotebook { .. }
    ));
}

#[test]
fn test_deduplicate_no_snapshots() {
    let original_events = vec![ModelEvent::SaveBlock(BlockCompleted {
        pane_id: vec![1, 2, 3],
        block: Default::default(),
        is_local: true,
    })];
    let filtered_events = deduplicate_events(original_events);
    assert_eq!(filtered_events.len(), 1);
    assert!(matches!(&filtered_events[0], &ModelEvent::SaveBlock(_)));
}

fn test_terminal_window_snapshot(vertical_tabs_panel_open: bool) -> WindowSnapshot {
    let root = PaneNodeSnapshot::Leaf(LeafSnapshot {
        is_focused: true,
        custom_vertical_tabs_title: None,
        contents: LeafContents::Terminal(TerminalPaneSnapshot {
            uuid: vec![u8::from(vertical_tabs_panel_open) + 1],
            cwd: Some("/tmp".to_string()),
            shell_launch_data: Some(ShellLaunchData::Executable {
                executable_path: PathBuf::from("/bin/zsh"),
                shell_type: crate::terminal::shell::ShellType::Zsh,
            }),
            is_active: true,
            is_read_only: false,
            input_config: None,
            llm_model_override: None,
            active_profile_id: None,
            conversation_ids_to_restore: vec![],
            active_conversation_id: None,
        }),
    });
    WindowSnapshot {
        tabs: vec![TabSnapshot {
            custom_title: None,
            terminal_tabs: TabSnapshot::single_terminal_tab(root.clone()),
            active_terminal_tab: 0,
            root,
            default_directory_color: None,
            selected_color: SelectedTabColor::default(),
            left_panel: None,
            right_panel: None,
            tcb_project_id: None,
        }],
        active_tab_index: 0,
        bounds: None,
        fullscreen_state: Default::default(),
        quake_mode: false,
        universal_search_width: None,
        mnemos_ai_width: None,
        voltron_width: None,
        mnemos_drive_index_width: None,
        left_panel_open: false,
        vertical_tabs_panel_open,
        left_panel_width: None,
        right_panel_width: None,
        agent_management_filters: None,
    }
}

#[test]
fn test_sqlite_round_trips_vertical_tabs_panel_open() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("mnemos.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    let app_state = AppState {
        windows: vec![
            test_terminal_window_snapshot(false),
            test_terminal_window_snapshot(true),
        ],
        active_window_index: Some(1),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };

    save_app_state(&mut conn, &app_state).expect("app state should save");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    assert_eq!(restored.active_window_index, Some(1));
    assert_eq!(
        restored
            .windows
            .iter()
            .map(|window| window.vertical_tabs_panel_open)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
}

#[test]
fn test_sqlite_round_trips_custom_vertical_tabs_title() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("mnemos.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    let app_state = AppState {
        windows: vec![WindowSnapshot {
            tabs: vec![{
                let root = PaneNodeSnapshot::Leaf(LeafSnapshot {
                    is_focused: true,
                    custom_vertical_tabs_title: Some("Production API".to_string()),
                    contents: LeafContents::Terminal(TerminalPaneSnapshot {
                        uuid: vec![42],
                        cwd: Some("/tmp".to_string()),
                        shell_launch_data: Some(ShellLaunchData::Executable {
                            executable_path: PathBuf::from("/bin/zsh"),
                            shell_type: crate::terminal::shell::ShellType::Zsh,
                        }),
                        is_active: true,
                        is_read_only: false,
                        input_config: None,
                        llm_model_override: None,
                        active_profile_id: None,
                        conversation_ids_to_restore: vec![],
                        active_conversation_id: None,
                    }),
                });
                TabSnapshot {
                    custom_title: None,
                    terminal_tabs: TabSnapshot::single_terminal_tab(root.clone()),
                    active_terminal_tab: 0,
                    root,
                    default_directory_color: None,
                    selected_color: SelectedTabColor::default(),
                    left_panel: None,
                    right_panel: None,
                    tcb_project_id: None,
                }
            }],
            active_tab_index: 0,
            bounds: None,
            fullscreen_state: Default::default(),
            quake_mode: false,
            universal_search_width: None,
            mnemos_ai_width: None,
            voltron_width: None,
            mnemos_drive_index_width: None,
            left_panel_open: false,
            vertical_tabs_panel_open: false,
            left_panel_width: None,
            right_panel_width: None,
            agent_management_filters: None,
        }],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };

    save_app_state(&mut conn, &app_state).expect("app state should save");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    let PaneNodeSnapshot::Leaf(LeafSnapshot {
        custom_vertical_tabs_title,
        ..
    }) = &restored.windows[0].tabs[0].root
    else {
        panic!("Expected terminal pane leaf");
    };
    assert_eq!(
        custom_vertical_tabs_title.as_deref(),
        Some("Production API")
    );
}

#[test]
fn test_sqlite_round_trips_code_pane_with_multiple_tabs() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("mnemos.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    let app_state = AppState {
        windows: vec![WindowSnapshot {
            tabs: vec![{
                let root = PaneNodeSnapshot::Leaf(LeafSnapshot {
                    is_focused: true,
                    custom_vertical_tabs_title: None,
                    contents: LeafContents::Code(CodePaneSnapShot::Local {
                        tabs: vec![
                            CodePaneTabSnapshot {
                                path: Some(PathBuf::from("/tmp/main.rs")),
                            },
                            CodePaneTabSnapshot {
                                path: Some(PathBuf::from("/tmp/lib.rs")),
                            },
                            CodePaneTabSnapshot { path: None },
                        ],
                        active_tab_index: 1,
                        source: Some(CodeSource::FileTree {
                            path: PathBuf::from("/tmp/main.rs"),
                        }),
                    }),
                });
                TabSnapshot {
                    custom_title: None,
                    terminal_tabs: TabSnapshot::single_terminal_tab(root.clone()),
                    active_terminal_tab: 0,
                    root,
                    default_directory_color: None,
                    selected_color: SelectedTabColor::default(),
                    left_panel: None,
                    right_panel: None,
                    tcb_project_id: None,
                }
            }],
            active_tab_index: 0,
            bounds: None,
            fullscreen_state: Default::default(),
            quake_mode: false,
            universal_search_width: None,
            mnemos_ai_width: None,
            voltron_width: None,
            mnemos_drive_index_width: None,
            left_panel_open: false,
            vertical_tabs_panel_open: false,
            left_panel_width: None,
            right_panel_width: None,
            agent_management_filters: None,
        }],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };

    save_app_state(&mut conn, &app_state).expect("app state should save");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    assert_eq!(restored.windows.len(), 1);
    let restored_tab = &restored.windows[0].tabs[0];
    let PaneNodeSnapshot::Leaf(LeafSnapshot {
        contents:
            LeafContents::Code(CodePaneSnapShot::Local {
                tabs,
                active_tab_index,
                source,
            }),
        ..
    }) = &restored_tab.root
    else {
        panic!("Expected code pane leaf");
    };

    assert_eq!(tabs.len(), 3);
    assert_eq!(*active_tab_index, 1);
    assert_eq!(tabs[0].path, Some(PathBuf::from("/tmp/main.rs")));
    assert_eq!(tabs[1].path, Some(PathBuf::from("/tmp/lib.rs")));
    assert_eq!(tabs[2].path, None);
    assert!(matches!(source, Some(CodeSource::FileTree { .. })));
}

/// Builds a single terminal leaf snapshot with the given cwd. Helper for the
/// terminal-tab round-trip test.
fn terminal_leaf(uuid: u8, cwd: &str) -> PaneNodeSnapshot {
    PaneNodeSnapshot::Leaf(LeafSnapshot {
        is_focused: true,
        custom_vertical_tabs_title: None,
        contents: LeafContents::Terminal(TerminalPaneSnapshot {
            uuid: vec![uuid],
            cwd: Some(cwd.to_string()),
            shell_launch_data: None,
            is_active: true,
            is_read_only: false,
            input_config: None,
            llm_model_override: None,
            active_profile_id: None,
            conversation_ids_to_restore: vec![],
            active_conversation_id: None,
        }),
    })
}

/// Acceptance test for browser-style terminal tabs: a project tab with three
/// terminal tabs (one of which is a horizontal split) round-trips through SQLite
/// preserving count, order, active tab, titles, and the per-tab pane trees.
#[test]
fn test_sqlite_round_trips_terminal_tabs() {
    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("mnemos.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    // Tab 1 is a horizontal split of two terminals; tabs 0 and 2 are single
    // terminals. Active tab is index 1.
    let split_root = PaneNodeSnapshot::Branch(BranchSnapshot {
        direction: SplitDirection::Horizontal,
        children: vec![
            (PaneFlex(1.0), terminal_leaf(11, "/tab1/left")),
            (PaneFlex(1.0), terminal_leaf(12, "/tab1/right")),
        ],
    });

    let terminal_tabs = vec![
        PaneTabSnapshot {
            id: uuid::Uuid::new_v4().as_bytes().to_vec(),
            title: Some("first".to_string()),
            root: terminal_leaf(1, "/tab0"),
        },
        PaneTabSnapshot {
            id: uuid::Uuid::new_v4().as_bytes().to_vec(),
            title: None, // derived title; should round-trip as None
            root: split_root,
        },
        PaneTabSnapshot {
            id: uuid::Uuid::new_v4().as_bytes().to_vec(),
            title: Some("third".to_string()),
            root: terminal_leaf(3, "/tab2"),
        },
    ];

    let active_terminal_tab = 1;
    // `root` mirrors the active tab's tree, matching the live snapshot producer.
    let root = terminal_tabs[active_terminal_tab].root.clone();

    let app_state = AppState {
        windows: vec![WindowSnapshot {
            tabs: vec![TabSnapshot {
                custom_title: None,
                terminal_tabs,
                active_terminal_tab,
                root,
                default_directory_color: None,
                selected_color: SelectedTabColor::default(),
                left_panel: None,
                right_panel: None,
                tcb_project_id: None,
            }],
            active_tab_index: 0,
            bounds: None,
            fullscreen_state: Default::default(),
            quake_mode: false,
            universal_search_width: None,
            mnemos_ai_width: None,
            voltron_width: None,
            mnemos_drive_index_width: None,
            left_panel_open: false,
            vertical_tabs_panel_open: false,
            left_panel_width: None,
            right_panel_width: None,
            agent_management_filters: None,
        }],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };

    save_app_state(&mut conn, &app_state).expect("app state should save");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    assert_eq!(restored.windows.len(), 1);
    let restored_tab = &restored.windows[0].tabs[0];

    // Count, order, and active index are preserved.
    assert_eq!(restored_tab.terminal_tabs.len(), 3);
    assert_eq!(restored_tab.active_terminal_tab, 1);

    // Titles round-trip (including the derived-title None case).
    assert_eq!(restored_tab.terminal_tabs[0].title.as_deref(), Some("first"));
    assert_eq!(restored_tab.terminal_tabs[1].title, None);
    assert_eq!(restored_tab.terminal_tabs[2].title.as_deref(), Some("third"));

    // Each tab's pane tree round-trips: tabs 0 and 2 are single terminals, tab 1
    // is a horizontal split of two.
    let cwd_of = |node: &PaneNodeSnapshot| -> Option<String> {
        match node {
            PaneNodeSnapshot::Leaf(LeafSnapshot {
                contents: LeafContents::Terminal(t),
                ..
            }) => t.cwd.clone(),
            _ => None,
        }
    };
    assert_eq!(
        cwd_of(&restored_tab.terminal_tabs[0].root),
        Some("/tab0".to_string())
    );
    assert_eq!(
        cwd_of(&restored_tab.terminal_tabs[2].root),
        Some("/tab2".to_string())
    );

    let PaneNodeSnapshot::Branch(branch) = &restored_tab.terminal_tabs[1].root else {
        panic!("Expected tab 1 to restore as a split branch");
    };
    assert_eq!(branch.direction, SplitDirection::Horizontal);
    assert_eq!(branch.children.len(), 2);
    assert_eq!(cwd_of(&branch.children[0].1), Some("/tab1/left".to_string()));
    assert_eq!(
        cwd_of(&branch.children[1].1),
        Some("/tab1/right".to_string())
    );

    // The compatibility `root` field points at the active tab's tree.
    assert!(matches!(restored_tab.root, PaneNodeSnapshot::Branch(_)));
}

fn assert_encode_then_decode_preserves_original_path(original_path: PathBuf) {
    let bytes = encode_path(original_path.clone());
    let decoded_path = decode_path(bytes);
    assert_eq!(original_path, decoded_path);
}

/// Test that a local path can be encoded and decoded. We use this when persisting a local
/// file path for notebooks in sqlite. We need this test because Windows `OsString`s are
/// often arbitrary sequences of 16-bit values, unlike Unix which uses sequences of 8-bit
/// values (bytes). Since `diesel::sql_types::Binary` deals with sequences of bytes (`u8`)
/// we need to perform special casting on `OsString`s on Windows.
#[test]
fn test_path_encode_decode() {
    // Empty path
    assert_encode_then_decode_preserves_original_path(PathBuf::new());

    // Windows-style paths
    assert_encode_then_decode_preserves_original_path(PathBuf::from(r"C:\windows\system32.dll"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from("c:temp"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from(r"\temp"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from(r"\temp\emoji\🙈.txt"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from(r"\temp\ñoñàscii\temp.txt"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from(r"\temp\hindi\हिन्दी"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from(r"\temp\cjk\狗没有耐心"));

    // Unix-style paths
    assert_encode_then_decode_preserves_original_path(PathBuf::from(
        "/home/persistence/example.sql",
    ));
    assert_encode_then_decode_preserves_original_path(PathBuf::from("./database/log.txt"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from("/temp/emoji/🙈.txt"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from("/temp/ñoñàscii/temp.txt"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from("/temp/hindi/हिन्दी"));
    assert_encode_then_decode_preserves_original_path(PathBuf::from("/temp/cjk/狗没有耐心"));
}

#[test]
fn test_deserialize_corrupted_guests() {
    let _ = FeatureFlag::SharedWithMe.override_enabled(true);
    // Use a hardcoded timestamp to ensure this test works on systems with more-than-microsecond
    // precision.
    let permissions_ts_micros = 123456;
    let permissions_ts =
        ServerTimestamp::from_unix_timestamp_micros(permissions_ts_micros).unwrap();

    let db_permissions = ObjectPermissions {
        id: 42,
        object_metadata_id: 10,
        subject_type: "TEAM".to_string(),
        subject_id: Some("7".to_string()),
        subject_uid: "team_uid12345678912345".to_string(),
        permissions_last_updated_at: Some(permissions_ts_micros),
        // This is not a valid set of encoded object guests.
        object_guests: Some(vec![1, 2, 3]),
        anyone_with_link_access_level: None,
        anyone_with_link_source: None,
    };

    // The overall permissions should successfully convert, minus the object guests.
    let cloud_permissions = super::to_cloud_object_permissions(&db_permissions, None);
    assert_eq!(
        cloud_permissions,
        Some(CloudObjectPermissions {
            owner: Owner::Team {
                team_uid: crate::server::ids::ServerId::from_string_lossy("team_uid12345678912345"),
            },
            permissions_last_updated_ts: Some(permissions_ts),
            anyone_with_link: None,
            guests: vec![],
        })
    );
}

/// A saved pane of a kind this build does not know (here `Browser`, the
/// removed browser pane) is dropped on restore; its sibling takes the split
/// and the tab still opens.
#[test]
fn test_sqlite_restore_drops_unknown_pane_kind() {
    use diesel::RunQueryDsl as _;

    let tempdir = tempfile::tempdir().expect("tempdir should be created");
    let database_path = tempdir.path().join("mnemos.sqlite");
    let mut conn = setup_database(&database_path).expect("database should initialize");

    let root = PaneNodeSnapshot::Branch(BranchSnapshot {
        direction: SplitDirection::Horizontal,
        children: vec![
            (PaneFlex(1.0), terminal_leaf(1, "/left")),
            (PaneFlex(1.0), terminal_leaf(2, "/right")),
        ],
    });
    let mut window = test_terminal_window_snapshot(false);
    window.tabs[0].terminal_tabs = TabSnapshot::single_terminal_tab(root.clone());
    window.tabs[0].root = root;
    let app_state = AppState {
        windows: vec![window],
        active_window_index: Some(0),
        block_lists: Default::default(),
        running_mcp_servers: Default::default(),
    };
    save_app_state(&mut conn, &app_state).expect("app state should save");

    // Rewrite the right pane as a `Browser` leaf with no kind-specific row,
    // the shape a build that saved one would have left.
    diesel::sql_query("PRAGMA foreign_keys = OFF")
        .execute(&mut conn)
        .expect("foreign keys should switch off");
    let changed = diesel::sql_query(
        "UPDATE pane_leaves SET kind = 'Browser' \
         WHERE pane_node_id = (SELECT id FROM terminal_panes WHERE cwd = '/right')",
    )
    .execute(&mut conn)
    .expect("rewriting the pane kind should work");
    assert_eq!(changed, 1);
    diesel::sql_query("DELETE FROM terminal_panes WHERE cwd = '/right'")
        .execute(&mut conn)
        .expect("dropping the terminal row should work");
    diesel::sql_query("PRAGMA foreign_keys = ON")
        .execute(&mut conn)
        .expect("foreign keys should switch back on");

    let restored = read_sqlite_data(&mut conn, None)
        .expect("app state should load")
        .app_state;

    let tab = &restored.windows[0].tabs[0];
    assert_eq!(tab.terminal_tabs.len(), 1);
    let PaneNodeSnapshot::Leaf(LeafSnapshot {
        contents: LeafContents::Terminal(terminal),
        ..
    }) = &tab.terminal_tabs[0].root
    else {
        panic!("expected the split to collapse to the remaining terminal");
    };
    assert_eq!(terminal.cwd.as_deref(), Some("/left"));
}
