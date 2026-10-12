use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use mnemos_core::channel::ChannelState;

use super::{cli_link_source, foreign_link_target, is_mnemos_path};

#[test]
fn mnemos_app_paths_are_recognised() {
    assert!(is_mnemos_path(Path::new(
        "/Applications/Mnemos.app/Contents/MacOS/mnemos"
    )));
    assert!(is_mnemos_path(Path::new(
        "/Users/x/target/debug/bundle/osx/MnemosDev.app/Contents/MacOS/dev"
    )));
    assert!(!is_mnemos_path(Path::new(
        "/Applications/Warp.app/Contents/Resources/bin/oz"
    )));
    assert!(!is_mnemos_path(Path::new("/opt/homebrew/bin/oz")));
}

#[test]
fn link_to_another_program_is_foreign() {
    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("other-oz");
    fs::write(&other, b"#!/bin/sh\n").unwrap();
    let link = dir.path().join("oz");
    symlink(&other, &link).unwrap();

    assert_eq!(foreign_link_target(&link), Some(other));
}

#[test]
fn dangling_or_missing_link_is_not_foreign() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("oz");
    assert_eq!(foreign_link_target(&link), None);

    symlink(dir.path().join("gone"), &link).unwrap();
    assert_eq!(foreign_link_target(&link), None);
}

#[test]
fn link_to_a_mnemos_bundle_is_not_foreign() {
    let dir = tempfile::tempdir().unwrap();
    let macos = dir.path().join("Mnemos.app/Contents/MacOS");
    fs::create_dir_all(&macos).unwrap();
    let binary = macos.join("mnemos");
    fs::write(&binary, b"").unwrap();
    let link = dir.path().join("oz");
    symlink(&binary, &link).unwrap();

    assert_eq!(foreign_link_target(&link), None);
}

#[test]
fn cli_link_prefers_the_bundled_wrapper() {
    let dir = tempfile::tempdir().unwrap();
    let contents = dir.path().join("Mnemos.app/Contents");
    let binary = contents.join("MacOS/mnemos");
    fs::create_dir_all(binary.parent().unwrap()).unwrap();
    fs::write(&binary, b"").unwrap();

    // Without the wrapper (a dev build), the link points at the binary.
    assert_eq!(cli_link_source(&binary), binary);

    let wrapper = contents
        .join("Resources/bin")
        .join(ChannelState::channel().cli_command_name());
    fs::create_dir_all(wrapper.parent().unwrap()).unwrap();
    fs::write(&wrapper, b"#!/bin/bash\n").unwrap();
    assert_eq!(cli_link_source(&binary), wrapper);
}
