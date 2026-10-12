//! Mnemos IDE: feeds arbitrary bytes, as any program in a pane can print
//! them, through the vte to ansi `Processor` with a handler that ignores
//! everything (`mnemos::terminal::model::ansi::fuzzing`).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    mnemos::terminal::model::ansi::fuzzing::parse_arbitrary(data);
});
