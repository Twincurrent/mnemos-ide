use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::sync::Arc;

use serde::Deserialize;
use serde_json as json;

use mnemos_ui::r#async::executor::Background;

use crate::terminal::color;
use crate::terminal::color::Colors;
use crate::terminal::event_listener::ChannelEventListener;
use crate::terminal::model::grid::{Dimensions, GridStorage};
use crate::terminal::model::{ansi, block::BlockSize, ObfuscateSecrets};
use crate::terminal::shell::{ShellName, ShellType};
use crate::terminal::ShellLaunchState;
use crate::terminal::{BlockPadding, SizeInfo, TerminalModel};

macro_rules! ref_tests {
    ($($name:ident)*) => {
        $(
            #[test]
            fn $name() {
                let test_dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src/terminal/ref_tests/data"));
                let test_path = test_dir.join(stringify!($name));
                ref_test(&test_path);
            }
        )*
    }
}

/// Reads the fixture `name` in `dir`. Mnemos IDE: the large fixtures (`grid.json`,
/// `alacritty.recording`) are stored gzipped as `<name>.gz` (66 MB of JSON shrinks to under
/// 1 MB), so read that when it exists and the plain file otherwise.
fn read_fixture(dir: &Path, name: &str) -> Vec<u8> {
    let gz_path = dir.join(format!("{name}.gz"));
    let mut res = Vec::new();
    if gz_path.exists() {
        flate2::read::GzDecoder::new(File::open(&gz_path).unwrap())
            .read_to_end(&mut res)
            .unwrap_or_else(|e| panic!("{}: {e}", gz_path.display()));
    } else {
        File::open(dir.join(name))
            .unwrap()
            .read_to_end(&mut res)
            .unwrap();
    }
    res
}

ref_tests! {
    csi_rep
    fish_cc
    indexed_256_colors
    issue_855
    ll
    newline_with_cursor_beyond_scroll_region
    tab_rendering
    tmux_git_log
    tmux_htop
    vim_24bitcolors_bce
    vim_large_window_scroll
    vim_simple_edit
    vttest_cursor_movement_1
    vttest_insert
    vttest_origin_mode_1
    vttest_origin_mode_2
    vttest_scroll
    vttest_tab_clear_set
    zsh_tab_completion
    history
    grid_reset
    zerowidth
    selective_erasure
    colored_reset
    delete_lines
    delete_chars_reset
    alt_reset
    deccolm_reset
    decaln_reset
    insert_blank_reset
    erase_chars_reset
    scroll_up_reset
    clear_underline
    region_scroll_down
    wrapline_alt_toggle
    saved_cursor
    saved_cursor_alt
    sgr
    underline
}

#[derive(Deserialize, Default)]
struct RefConfig {
    history_size: u32,
}

fn ref_test(dir: &Path) {
    let recording = read_fixture(dir, "alacritty.recording");
    let serialized_size = fs::read_to_string(dir.join("size.json")).unwrap();
    let serialized_grid = read_fixture(dir, "grid.json");
    let serialized_cfg = fs::read_to_string(dir.join("config.json")).unwrap();

    let size: SizeInfo = json::from_str(&serialized_size).unwrap();
    let grid: GridStorage = json::from_slice(&serialized_grid).unwrap();
    let ref_config: RefConfig = json::from_str(&serialized_cfg).unwrap();

    let history_size = ref_config.history_size;

    let (wakeups_tx, _) = async_channel::unbounded();
    let (events_tx, _) = async_channel::unbounded();
    let (pty_reads_tx, _) = async_broadcast::broadcast(1000);

    let channel_event_proxy = ChannelEventListener::new(wakeups_tx, events_tx, pty_reads_tx);
    let block_padding = BlockPadding {
        padding_top: 1.1,
        command_padding_top: 0.19,
        middle: 0.5,
        bottom: 1.,
    };

    let sizes = BlockSize {
        block_padding,
        size,
        max_block_scroll_limit: history_size as usize,
        mnemos_prompt_height_lines: 0.1,
    };
    let mut terminal = TerminalModel::new(
        None, /* restored_blocks */
        sizes,
        color::List::from(&Colors::default()),
        channel_event_proxy,
        Arc::new(Background::default()),
        false, /* should_show_bootstrap_block */
        false, /* should_show_in_band_command_blocks */
        false, /* should_show_memory_stats */
        false, /* honor_ps1 */
        false, /* is_inverted */
        ObfuscateSecrets::No,
        false, /* is_telemetry_enabled */
        None,  /* session_startup_path */
        ShellLaunchState::ShellSpawned {
            available_shell: None,
            display_name: ShellName::blank(),
            shell_type: ShellType::Bash,
        },
    );
    terminal.start_command_execution();
    // As an implementation detail of this test, we start this block as a background block, so
    // the shell output is directed to the output grid, rather than the command grid. We override
    // the command grid's styling to be bolded, so it's not suitable for these shell output parsing
    // tests.
    terminal.start_active_block_as_background_block();

    let mut parser = ansi::Processor::new();

    parser.parse_bytes(&mut terminal, &recording, &mut io::sink());

    let term_grid = terminal.raw_grid_for_ref_tests();

    // Assert the grid has the amount of content we expect in the active region
    // and in scrollback.
    assert_eq!(term_grid.total_rows(), grid.total_rows());
    assert_eq!(term_grid.visible_rows(), grid.visible_rows());
    assert_eq!(term_grid.history_size(), grid.history_size());

    let mut grids_equal = true;
    for i in 0..grid.total_rows() {
        let row = term_grid
            .row(i)
            .expect("should have a row for every row in the test grid");
        for j in 0..grid.columns() {
            let cell = &row[j];
            let original_cell = &grid[i][j];
            if original_cell != cell {
                grids_equal = false;
                println!("[{i}][{j}] {original_cell:?} => {cell:?}",);
                println!("[{i}][{j}] {original_cell:?} => {cell:?}",);
                println!("[{i}][{j}] {original_cell:?} => {cell:?}",);
            }
        }
    }

    assert!(grids_equal, "Ref test failed; grid doesn't match");
}
