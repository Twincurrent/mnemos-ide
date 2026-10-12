use mnemos_ui::platform::OperatingSystem;
use mnemos_ui::{
    keymap::{EditableBinding, Keystroke, Trigger},
    App,
};

use crate::{
    util::bindings::{custom_tag_to_keystroke, keybinding_name_to_display_string, CustomAction},
    workspace::WorkspaceAction,
};

#[test]
fn test_keybinding_name_to_display_string() {
    App::test((), |mut app| async move {
        app.update(|ctx| {
            ctx.register_editable_bindings([
                EditableBinding::new(
                    "workspace:show_settings",
                    "Open settings",
                    WorkspaceAction::ShowSettings,
                )
                .with_key_binding("cmd-,"),
                EditableBinding::new(
                    "workspace:toggle_resource_center",
                    "Toggle Resource Center",
                    WorkspaceAction::ToggleResourceCenter,
                ),
            ]);

            let displayed_keybinding = if OperatingSystem::get().is_mac() {
                "⌘,"
            } else {
                "Logo ,"
            };
            assert_eq!(
                Some(displayed_keybinding),
                keybinding_name_to_display_string("workspace:show_settings", ctx).as_deref()
            );

            assert_eq!(
                None,
                keybinding_name_to_display_string("workspace:toggle_resource_center", ctx)
            );

            ctx.set_custom_trigger(
                "workspace:show_settings".to_owned(),
                Trigger::Keystrokes(vec![Keystroke::parse("cmd-shift-<").unwrap()]),
            );

            let displayed_keybinding = if OperatingSystem::get().is_mac() {
                "⇧⌘<"
            } else {
                "Shift Logo <"
            };
            assert_eq!(
                Some(displayed_keybinding),
                keybinding_name_to_display_string("workspace:show_settings", ctx).as_deref()
            );

            ctx.set_custom_trigger(
                "workspace:toggle_resource_center".to_owned(),
                Trigger::Keystrokes(vec![Keystroke::parse("cmd-alt-/").unwrap()]),
            );

            let expected_keybinding = if OperatingSystem::get().is_mac() {
                "⌥⌘/"
            } else {
                "Alt Logo /"
            };
            assert_eq!(
                Some(expected_keybinding),
                keybinding_name_to_display_string("workspace:toggle_resource_center", ctx)
                    .as_deref()
            );
        });
    });
}

/// Mnemos IDE: plain Ctrl+T and Ctrl+W belong to the PTY on Windows and Linux (fzf,
/// readline transpose-chars and unix-word-rubout, vim, Claude Code), so no custom
/// action may take them by default there. New tab and close tab are Ctrl+Shift+T and
/// Ctrl+Shift+W; on Mac they are Cmd+T and Cmd+W.
#[test]
fn new_and_close_tab_leave_plain_ctrl_t_and_ctrl_w_to_the_pty() {
    let ctrl_t = Keystroke::parse("ctrl-t").unwrap();
    let ctrl_w = Keystroke::parse("ctrl-w").unwrap();
    let is_mac = OperatingSystem::get().is_mac();

    if !is_mac {
        for action in enum_iterator::all::<CustomAction>() {
            let keystroke = custom_tag_to_keystroke(action.into());
            assert_ne!(keystroke.as_ref(), Some(&ctrl_t), "{action:?} takes ctrl-t");
            assert_ne!(keystroke.as_ref(), Some(&ctrl_w), "{action:?} takes ctrl-w");
        }
    }

    let (new_tab, close_tab) = if is_mac {
        ("cmd-t", "cmd-w")
    } else {
        ("ctrl-shift-T", "ctrl-shift-W")
    };
    assert_eq!(
        custom_tag_to_keystroke(CustomAction::NewTab.into()),
        Keystroke::parse(new_tab).ok()
    );
    assert_eq!(
        custom_tag_to_keystroke(CustomAction::CloseCurrentSession.into()),
        Keystroke::parse(close_tab).ok()
    );
}
