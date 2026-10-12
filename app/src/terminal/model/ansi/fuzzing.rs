//! Mnemos IDE: a way to run the vte to ansi [`Processor`] on arbitrary bytes
//! with a handler that ignores everything, for `fuzz/` (cargo-fuzz) and the
//! in-tree escape soup test. A panic here takes the whole app down from one
//! line of program output, as `printf '\e]1337\a'` once did.
#![allow(unused_imports)]

use std::collections::HashMap;
use std::io;

use mnemos_terminal::model::ansi::control_sequence_parameters::*;
use mnemos_terminal::model::{KeyboardModes, KeyboardModesApplyBehavior};
use mnemos_ui::color::ColorU;

use super::dcs_hooks::*;
use super::ProcessorInput;
use super::{Handler, Processor};
use crate::terminal::model::completions::{ShellCompletion, ShellCompletionUpdate};
use crate::terminal::model::image_map::StoredImageMetadata;
use crate::terminal::model::iterm_image::{ITermImage, ITermImageMetadata};
use crate::terminal::model::kitty::{KittyAction, KittyChunk, KittyResponse};
use crate::terminal::model::terminal_model::TmuxInstallationState;
use crate::terminal::model::{
    completions::ShellData as CompletionsShellData, index::VisibleRow, selection::ScrollDelta,
    tmux::ControlModeEvent,
};

/// A [`Handler`] that does nothing. Only the methods without a default are
/// listed; the parser itself is what is under test.
#[derive(Default)]
pub struct NoopHandler;

impl Handler for NoopHandler {
    fn set_title(&mut self, _: Option<String>) {}
    fn set_cursor_style(&mut self, _: Option<CursorStyle>) {}
    fn set_cursor_shape(&mut self, _shape: CursorShape) {}
    fn input(&mut self, _c: char) {}
    fn goto(&mut self, _: VisibleRow, _: usize) {}
    fn goto_line(&mut self, _: VisibleRow) {}
    fn goto_col(&mut self, _: usize) {}
    fn insert_blank(&mut self, _: usize) {}
    fn move_up(&mut self, _: usize) {}
    fn move_down(&mut self, _: usize) {}
    fn identify_terminal<W: io::Write>(&mut self, _: &mut W, _intermediate: Option<char>) {}
    fn report_xtversion<W: io::Write>(&mut self, _: &mut W) {}
    fn device_status<W: io::Write>(&mut self, _: &mut W, _: usize) {}
    fn move_forward(&mut self, _: usize) {}
    fn move_backward(&mut self, _: usize) {}
    fn move_down_and_cr(&mut self, _: usize) {}
    fn move_up_and_cr(&mut self, _: usize) {}
    fn put_tab(&mut self, _count: u16) {}
    fn backspace(&mut self) {}
    fn carriage_return(&mut self) {}
    fn linefeed(&mut self) -> ScrollDelta {
        ScrollDelta::Down { lines: 0 }
    }
    fn bell(&mut self) {}
    fn substitute(&mut self) {}
    fn newline(&mut self) {}
    fn set_horizontal_tabstop(&mut self) {}
    fn scroll_up(&mut self, _: usize) -> ScrollDelta {
        ScrollDelta::Down { lines: 0 }
    }
    fn scroll_down(&mut self, _: usize) -> ScrollDelta {
        ScrollDelta::Down { lines: 0 }
    }
    fn insert_blank_lines(&mut self, _: usize) -> ScrollDelta {
        ScrollDelta::Down { lines: 0 }
    }
    fn delete_lines(&mut self, _: usize) -> ScrollDelta {
        ScrollDelta::Down { lines: 0 }
    }
    fn erase_chars(&mut self, _: usize) {}
    fn delete_chars(&mut self, _: usize) {}
    fn move_backward_tabs(&mut self, _count: u16) {}
    fn move_forward_tabs(&mut self, _count: u16) {}
    fn save_cursor_position(&mut self) {}
    fn restore_cursor_position(&mut self) {}
    fn clear_line(&mut self, _mode: LineClearMode) {}
    fn clear_screen(&mut self, _mode: ClearMode) {}
    fn clear_tabs(&mut self, _mode: TabulationClearMode) {}
    fn reset_state(&mut self) {}
    fn reverse_index(&mut self) -> ScrollDelta {
        ScrollDelta::Down { lines: 0 }
    }
    fn terminal_attribute(&mut self, _attr: Attr) {}
    fn set_mode(&mut self, _mode: Mode) {}
    fn unset_mode(&mut self, _: Mode) {}
    fn set_keyboard_enhancement_flags(
        &mut self,
        _mode: KeyboardModes,
        _apply: KeyboardModesApplyBehavior,
    ) {
    }
    fn push_keyboard_enhancement_flags(&mut self, _mode: KeyboardModes) {}
    fn pop_keyboard_enhancement_flags(&mut self, _count: u16) {}
    fn query_keyboard_enhancement_flags<W: io::Write>(&mut self, _writer: &mut W) {}
    fn set_scrolling_region(&mut self, _top: usize, _bottom: Option<usize>) {}
    fn set_keypad_application_mode(&mut self) {}
    fn unset_keypad_application_mode(&mut self) {}
    fn set_active_charset(&mut self, _: CharsetIndex) {}
    fn configure_charset(&mut self, _: CharsetIndex, _: StandardCharset) {}
    fn set_color(&mut self, _: usize, _: ColorU) {}
    fn dynamic_color_sequence<W: io::Write>(&mut self, _: &mut W, _: u8, _: usize, _: &str) {}
    fn reset_color(&mut self, _: usize) {}
    fn clipboard_store(&mut self, _: u8, _: &[u8]) {}
    fn clipboard_load(&mut self, _: u8, _: &str) {}
    fn decaln(&mut self) {}
    fn push_title(&mut self) {}
    fn pop_title(&mut self) {}
    fn text_area_size_pixels<W: io::Write>(&mut self, _: &mut W) {}
    fn text_area_size_chars<W: io::Write>(&mut self, _: &mut W) {}
}

/// Parse `bytes` as PTY output, split in two at a point the first byte picks,
/// so state carried across reads is exercised too.
pub fn parse_arbitrary(bytes: &[u8]) {
    let mut processor = Processor::new();
    let split = bytes.first().map_or(0, |&b| b as usize % (bytes.len() + 1));
    processor.parse_bytes(&mut NoopHandler, &bytes[..split], &mut io::sink());
    processor.parse_bytes(&mut NoopHandler, &bytes[split..], &mut io::sink());
}
