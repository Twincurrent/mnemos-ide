// We can use `std::process:Command` here because this is invoked within a build script,
// _not_ within the Warp binary (where it could cause a terminal to temporarily flash on
// Windows).
#![allow(clippy::disallowed_types)]

use cfg_aliases::cfg_aliases;

use anyhow::Result;
use sha2::Digest;
use std::path::{Path, PathBuf};
use std::{env, fs, process::Command};
use walkdir::WalkDir;
use mnemos_util::assets::{
    ASSETS_DIR, ASYNC_ASSETS_DIR, CONPTY_DLL_FILE, DXCOMPILER_DLL_FILE, DXIL_DLL_FILE,
    OPEN_CONSOLE_EXE_FILE, REMOTE_ASSETS_DIR, WINDOWS_ASSETS_DIR,
};
use mnemos_util::path::app_target_dir;

fn main() -> Result<()> {
    cfg_aliases! {
        linux_or_windows: { any(target_os = "linux", windows) },
        enable_crash_recovery: { linux_or_windows },
    }

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_OS");
    println!("cargo:rerun-if-env-changed=CARGO_CFG_TARGET_FAMILY");

    let target_os = env::var("CARGO_CFG_TARGET_OS")?;
    let target_family = env::var("CARGO_CFG_TARGET_FAMILY")?;

    add_features(&target_family, &target_os);
    embed_source_info();

    if target_os == "macos" && target_family != "wasm" {
        println!("cargo:rustc-link-lib=framework=MetalKit");
        println!("cargo:rustc-link-lib=framework=UserNotifications");
        build_and_link_sentry();

        println!("cargo:rerun-if-changed=src/platform/mac/objc/app_bundle.h");
        println!("cargo:rerun-if-changed=src/platform/mac/objc/app_bundle.m");
        println!("cargo:rerun-if-changed=src/platform/mac/objc/services.h");
        println!("cargo:rerun-if-changed=src/platform/mac/objc/services.m");

        cc::Build::new()
            .file("src/platform/mac/objc/app_bundle.m")
            .file("src/platform/mac/objc/services.m")
            .compile("warp_objc");

        // Build the dock tile plugin
        println!("cargo:rerun-if-changed=DockTilePlugin/MnemosDockTilePlugin.m");
        println!("cargo:rerun-if-changed=DockTilePlugin/MnemosDockTilePlugin.h");
        println!("cargo:rerun-if-changed=DockTilePlugin/Info.plist");
        println!("cargo:rerun-if-changed=DockTilePlugin/Makefile");
        println!("cargo:rerun-if-changed=DockTilePlugin/Resources");

        let min_macos_version = env::var("MACOSX_DEPLOYMENT_TARGET")
            .expect("MACOSX_DEPLOYMENT_TARGET must be set for macos builds");
        let status = Command::new("make")
            .current_dir("DockTilePlugin")
            .env("MACOSX_DEPLOYMENT_TARGET", min_macos_version)
            .status()
            .expect("Failed to build dock tile plugin");
        if !status.success() {
            panic!("Dock tile plugin build failed");
        }

        // Copy the dock tile plugin to the output directory
        let profile = get_build_profile_name();
        let target_dir = app_target_dir(&profile).expect("Failed to get app target directory");
        let plugin_src = Path::new("DockTilePlugin/MnemosDockTilePlugin.docktileplugin");
        let plugin_dst = target_dir.join("MnemosDockTilePlugin.docktileplugin");

        if !status.success() {
            fs::remove_dir_all(plugin_src).expect("Failed to clean up plugin directory");
            panic!("Dock tile plugin build failed");
        }

        if plugin_src.exists() {
            fs::remove_dir_all(&plugin_dst).ok(); // Remove existing if any
            fs::create_dir_all(&plugin_dst).expect("Failed to create plugin directory");

            // Copy the plugin directory recursively
            for entry in WalkDir::new(plugin_src) {
                let entry = entry.expect("Failed to read plugin directory");
                let path = entry.path();
                let relative = path
                    .strip_prefix(plugin_src)
                    .expect("Failed to strip path prefix");
                let target = plugin_dst.join(relative);

                if path.is_dir() {
                    fs::create_dir_all(target).expect("Failed to create plugin subdirectory");
                } else {
                    fs::copy(path, target).expect("Failed to copy plugin file");
                }
            }

            // Clean up the source plugin directory after copying
            fs::remove_dir_all(plugin_src).expect("Failed to clean up plugin directory");
        }

        // In standalone mode, embed the Info.plist file. We don't use embed_plist! for this
        // because the plist file is dynamically generated.
        if env::var("CARGO_FEATURE_STANDALONE").is_ok() {
            // Don't fail if INFO_PLIST_PATH is unset, since CI runs clippy with --all-features.
            if let Ok(info_plist_path) = env::var("INFO_PLIST_PATH") {
                println!("cargo:rerun-if-env-changed=INFO_PLIST_PATH");
                println!("cargo:rerun-if-changed={info_plist_path}");
                println!("cargo:rustc-link-arg=-sectcreate");
                println!("cargo:rustc-link-arg=__TEXT");
                println!("cargo:rustc-link-arg=__info_plist");
                println!("cargo:rustc-link-arg={info_plist_path}");
            } else {
                eprintln!("Expected INFO_PLIST_PATH to be set")
            }
        }
    }

    if target_os == "windows" {
        // Retrieve the Cargo profile name so that we can put a copy of ConPTY in
        // the correct target subdirectory.
        //
        // We need to pass this information manually through an environment variable.
        // Of the built-in variables set by Cargo: `OUT_DIR` is only a temporary
        // directory, and `PROFILE` can only be `debug` or `release`.
        // See https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-build-scripts
        // for more on Cargo environment variables.
        //
        // Ideally we could access `CARGO_TARGET_DIR` but this doesn't exist at build time.
        // See https://github.com/rust-lang/cargo/issues/9661.
        //
        // Cargo defaults to the `debug` profile.
        let cargo_full_profile = env::var("CARGO_FULL_PROFILE").unwrap_or(String::from("debug"));
        let target_dir =
            app_target_dir(&cargo_full_profile).expect("Could not get app target directory");
        copy_windows_assets(&target_dir);

        #[cfg(windows)]
        embed_resource_file(&target_dir);
    }

    if target_family == "wasm" {
        copy_async_assets();
    }

    generate_channel_config(&target_family, &target_os);

    Ok(())
}

/// Writes `OUT_DIR/stable_config.json`, which `load_config!` (src/bin/channel_config.rs) embeds
/// with `include_str!`.
///
/// `release_bundle` builds always load it and get the production config. Other builds use the
/// optional out-of-repo `mnemos-channel-config` generator when it is on PATH and fall back to this
/// file, which then holds a dev config: its own app id and no autoupdate. Cargo runs the build
/// script separately per feature set, so the two variants never share an `OUT_DIR`.
///
/// The dev app id only separates a dev build from the installed app when the binary does not run
/// from a bundle (`cargo run` on Windows, a bare binary on macOS). It never does on Linux, where
/// `directories::ProjectDirs` keeps only the lowercased app name, `mnemos` for both ids. On macOS,
/// `ChannelState::new` replaces the config's app id with the bundle identifier, and the `.app`
/// that `./script/run` builds carries the installed app's identifier (`dev.mnemos.Mnemos`, from
/// `[package.metadata.bundle.bin.mnemos]` in app/Cargo.toml). Such a run shares the installed
/// app's app id, UserDefaults domain and unprofiled data domain; only `MNEMOS_DATA_PROFILE` (debug
/// builds) and `MNEMOS_DATA_DIR` (or the legacy `TCB_DATA_DIR`) keep its data apart.
fn generate_channel_config(target_family: &str, target_os: &str) {
    let _ = target_family;
    let _ = target_os;

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR must be set");

    let (app_id, autoupdate_config) = if env::var("CARGO_FEATURE_RELEASE_BUNDLE").is_ok() {
        println!("cargo:rerun-if-env-changed=WITH_LOCAL_SERVER");
        println!("cargo:rerun-if-env-changed=WITH_LOCAL_SESSION_SHARING_SERVER");
        println!("cargo:rerun-if-env-changed=WITH_SANDBOX_TELEMETRY");
        println!("cargo:rerun-if-env-changed=SERVER_ROOT_URL");
        println!("cargo:rerun-if-env-changed=WS_SERVER_URL");
        (
            "dev.mnemos.Mnemos",
            autoupdate_config_json().unwrap_or_else(|| "null".to_owned()),
        )
    } else {
        // Same app id as the private generator emits for dev builds. It takes effect only when
        // the binary is not in a bundle; see the doc comment above.
        ("dev.twincurrent.mnemos", "null".to_owned())
    };

    // The original Warp build invokes an internal `warp-channel-config` / `mnemos-channel-config`
    // generator that isn't shipped with this fork. Emit a baked-in stable config that points at
    // the production TwinCurrent endpoints so builds work without the generator.
    let stable_config = format!(
        r#"{{
  "app_id": "{app_id}",
  "logfile_name": "mnemos.log",
  "server_config": {{
    "server_root_url": "https://admin.twincurrent.dk",
    "rtc_server_url": "wss://admin.twincurrent.dk/rtc",
    "session_sharing_server_url": "wss://admin.twincurrent.dk/sessions",
    "firebase_auth_api_key": "",
    "terms_of_service_url": null,
    "privacy_policy_url": null
  }},
  "oz_config": {{
    "oz_root_url": "https://admin.twincurrent.dk",
    "workload_audience_url": null
  }},
  "telemetry_config": null,
  "autoupdate_config": {autoupdate_config},
  "crash_reporting_config": null,
  "mcp_static_config": null
}}
"#
    );

    let config_path = Path::new(&out_dir).join("stable_config.json");
    fs::write(&config_path, stable_config).unwrap_or_else(|err| {
        panic!("Failed to write config to {}: {err}", config_path.display())
    });
}

/// Mnemos IDE: the `autoupdate_config` of a `release_bundle` build, or `None` when the build must
/// not update itself.
///
/// Only official releases update by default: `MNEMOS_OFFICIAL_RELEASE=1` (set by
/// script/macos/release and script/windows/release.ps1) turns the updater on against
/// TwinCurrent's release host. A fork turns it on against its own host by setting
/// `RELEASES_BASE_URL`. Every other build, `script/bundle` from a source checkout included, gets no
/// updater, so it never replaces itself with somebody else's binary. `MNEMOS_UPDATER=off` turns it
/// off even for an official build (for packagers who ship updates themselves; see PACKAGING.md).
///
/// An updater is only emitted together with the public keys its manifests must be signed with
/// (`MNEMOS_UPDATE_PUBLIC_KEYS`, else the key lines of script/update-signing.pub). A build that
/// would update without a key panics here instead of shipping an updater that refuses everything.
fn autoupdate_config_json() -> Option<String> {
    println!("cargo:rerun-if-env-changed=MNEMOS_OFFICIAL_RELEASE");
    println!("cargo:rerun-if-env-changed=MNEMOS_UPDATER");
    println!("cargo:rerun-if-env-changed=RELEASES_BASE_URL");
    println!("cargo:rerun-if-env-changed=MNEMOS_UPDATE_PUBLIC_KEYS");
    println!("cargo:rerun-if-changed=../script/update-signing.pub");

    let non_empty = |name: &str| env::var(name).ok().filter(|value| !value.trim().is_empty());
    if non_empty("MNEMOS_UPDATER").is_some_and(|value| value.trim().eq_ignore_ascii_case("off")) {
        return None;
    }
    let official = non_empty("MNEMOS_OFFICIAL_RELEASE").is_some_and(|value| value.trim() == "1");
    // The base URL is baked into every shipped binary and can never be changed for installs
    // already in the wild, so it has to be a host the publisher controls permanently. TwinCurrent's
    // points at the admin platform, which redirects to whichever bucket holds the bytes.
    let releases_base_url = match non_empty("RELEASES_BASE_URL") {
        Some(url) => url.trim().trim_end_matches('/').to_owned(),
        None if official => "https://admin.twincurrent.dk/releases".to_owned(),
        None => return None,
    };

    let key_text = match non_empty("MNEMOS_UPDATE_PUBLIC_KEYS") {
        Some(keys) => keys.replace(',', "\n"),
        None => fs::read_to_string("../script/update-signing.pub").unwrap_or_default(),
    };
    let keys: Vec<&str> = key_text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    if keys.is_empty() {
        panic!(
            "This build turns the updater on (MNEMOS_OFFICIAL_RELEASE=1 or RELEASES_BASE_URL) but \
             has no update signing public key. Add one to script/update-signing.pub or set \
             MNEMOS_UPDATE_PUBLIC_KEYS (see RELEASING.md, \"Update signing\"), or build with \
             MNEMOS_UPDATER=off."
        );
    }
    for key in &keys {
        // `mnemos-update-pub:` + base64 of 40 bytes (56 characters). Checked again, fully, by
        // the client; this only catches a pasted secret key or a truncated line.
        let encoded = key.strip_prefix("mnemos-update-pub:").unwrap_or_else(|| {
            panic!("Update signing public key {key:?} does not start with mnemos-update-pub:")
        });
        if encoded.len() != 56
            || !encoded
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
        {
            panic!("Update signing public key {key:?} is not a mnemos-update-pub key");
        }
    }
    let keys_json = keys
        .iter()
        .map(|key| format!("\"{key}\""))
        .collect::<Vec<_>>()
        .join(", ");
    if releases_base_url.contains('"') || releases_base_url.contains('\\') {
        panic!("RELEASES_BASE_URL must not contain quotes or backslashes");
    }
    Some(format!(
        r#"{{
    "releases_base_url": "{releases_base_url}",
    "show_autoupdate_menu_items": true,
    "update_public_keys": [{keys_json}]
  }}"#
    ))
}

/// Mnemos IDE: the version shown to people, as semver: the app crate's version with the release
/// stamp as build metadata (`0.1.0+2026.10.09.12.30.stable.01`), or, for a build without
/// `GIT_RELEASE_TAG`, `+source` and the short commit. The release tag itself stays the machine
/// version that orders updates.
fn embed_display_version(commit: &str) {
    println!("cargo:rerun-if-env-changed=GIT_RELEASE_TAG");
    let package_version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_owned());
    let metadata = match env::var("GIT_RELEASE_TAG")
        .ok()
        .filter(|tag| !tag.trim().is_empty())
    {
        Some(tag) => tag
            .trim()
            .trim_start_matches("v0.")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' {
                    c
                } else {
                    '.'
                }
            })
            .collect::<String>(),
        None => {
            let short: String = commit.chars().take(12).collect();
            if short.chars().all(|c| c.is_ascii_hexdigit()) && !short.is_empty() {
                format!("source.{short}")
            } else {
                "source".to_owned()
            }
        }
    };
    println!("cargo:rustc-env=MNEMOS_DISPLAY_VERSION={package_version}+{metadata}");
}

fn get_build_profile_name() -> String {
    // The profile name is always the 3rd last part of the path (with 1 based indexing).
    // e.g. /code/core/target/cli/build/my-build-info-9f91ba6f99d7a061/out
    env::var("OUT_DIR")
        .expect("OUT_DIR must be set")
        .split(std::path::MAIN_SEPARATOR)
        .nth_back(3)
        .expect("could not get profile name")
        .to_string()
}

/// Mnemos IDE: AGPL-3.0 section 6. Embeds where this binary's source is published
/// (`MNEMOS_SOURCE_URL`, else the first non-comment line of `script/source-url`) and the commit it
/// was built from (`MNEMOS_GIT_COMMIT`, else `git rev-parse HEAD`), for the About page.
///
/// Builds with the `debug` profile skip git unless `MNEMOS_GIT_COMMIT` is set: tracking HEAD would
/// rebuild the whole app crate after every commit. Release scripts refuse a dirty tree, so a
/// release's commit is exactly its source.
fn embed_source_info() {
    println!("cargo:rerun-if-env-changed=MNEMOS_SOURCE_URL");
    println!("cargo:rerun-if-env-changed=MNEMOS_GIT_COMMIT");
    println!("cargo:rerun-if-changed=../script/source-url");

    let source_url = env::var("MNEMOS_SOURCE_URL")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            fs::read_to_string("../script/source-url").ok().and_then(|s| {
                s.lines()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && !l.starts_with('#'))
                    .map(str::to_owned)
            })
        })
        .unwrap_or_default();
    let source_url = source_url.trim().trim_end_matches('/');

    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .filter(|s| !s.is_empty())
    };
    let commit = match env::var("MNEMOS_GIT_COMMIT").ok().filter(|v| !v.trim().is_empty()) {
        Some(commit) => commit.trim().to_owned(),
        None if get_build_profile_name() == "debug" => String::new(),
        None => {
            // HEAD's reflog changes on every commit and checkout, also in a worktree, where
            // `--git-path` resolves to the worktree's own git dir. Only watch it when it exists:
            // a missing path would rerun this script on every build.
            if let Some(log) = git(&["rev-parse", "--git-path", "logs/HEAD"]) {
                if Path::new(&log).exists() {
                    println!("cargo:rerun-if-changed={log}");
                }
            }
            git(&["rev-parse", "HEAD"]).unwrap_or_default()
        }
    };

    println!("cargo:rustc-env=MNEMOS_SOURCE_URL={source_url}");
    println!("cargo:rustc-env=MNEMOS_GIT_COMMIT={commit}");
    embed_display_version(&commit);
}

fn add_features(target_family: &str, target_os: &str) {
    if target_family != "wasm" {
        println!("cargo:rustc-cfg=feature=\"local_fs\"");
        println!("cargo:rustc-cfg=feature=\"local_tty\"");
    }

    if target_os != "windows" {
        println!("cargo:rustc-cfg=feature=\"iterm_images\"");
    }

    if env::var("PROFILE").ok().is_some_and(|val| val == "debug") {
        println!("cargo:rustc-cfg=feature=\"agent_mode_debug\"");
    }
}

fn build_and_link_sentry() {
    // Ensure we re-run the build script if the target framework directory changes.
    println!("cargo:rerun-if-env-changed=FRAMEWORK_OVERRIDE");

    // If the cocoa_sentry feature is not enabled, there's nothing more to do here.
    if env::var("CARGO_FEATURE_COCOA_SENTRY").is_err() {
        return;
    }

    // Download/update the Sentry framework.
    let dir_name = env::var("FRAMEWORK_OVERRIDE").unwrap_or_else(|_| "default".to_string());
    let frameworks_dir = format!("frameworks/{dir_name}");
    let standalone = env::var("CARGO_FEATURE_STANDALONE").is_ok();
    download_sentry_framework(&frameworks_dir, &dir_name, standalone);

    let sentry_dir = if standalone {
        "Sentry.xcframework"
    } else {
        "Sentry-Dynamic-WithARM64e.xcframework"
    };
    let sentry_framework_path = format!("{frameworks_dir}/{sentry_dir}/macos-arm64_arm64e_x86_64");

    // Make sure we re-run the build script if the framework directory changes (e.g.: it gets
    // deleted).
    println!("cargo:rerun-if-changed={sentry_framework_path}");

    // Link the Sentry framework, and compile our objc logic that interfaces with it.
    println!("cargo:rustc-link-search=framework=app/{sentry_framework_path}");
    println!("cargo:rustc-link-lib=framework=Sentry");

    // If building standalone, we need to copy some flags from the Sentry build that the static library requires.
    if standalone {
        println!("cargo:rustc-link-lib=c++");
        let swift_library_path = get_xcode_toolchain().join("usr/lib/swift/macosx");
        println!(
            "cargo:rustc-link-search=all={}",
            swift_library_path.display()
        );
    }

    compile_sentry_objc_lib(&sentry_framework_path);
}

fn download_sentry_framework(frameworks_dir: &str, dir_name: &str, standalone: bool) {
    // Build absolute path to the script from workspace root.
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let workspace_root = manifest_dir.parent().unwrap();
    let script_path = workspace_root.join("script/macos/update_sentry_cocoa");

    let cocoa_sentry_version = match dir_name {
        "default" | "dev" => "9.4.1",
        name => panic!("Invalid framework override: {name}"),
    };

    let mut cmd = Command::new(&script_path);
    cmd.current_dir(workspace_root)
        .arg("--dir")
        .arg(format!("app/{frameworks_dir}"))
        .arg("--version")
        .arg(cocoa_sentry_version);

    if standalone {
        cmd.arg("--static");
    }

    let output = cmd
        .output()
        .expect("Failed to run update_sentry_cocoa script");
    if !output.status.success() {
        panic!(
            "Failed to download/update Sentry frameworks:\n--- stdout\n{}\n--- stderr\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn compile_sentry_objc_lib(sentry_framework_path: &str) {
    println!("cargo:rerun-if-changed=src/platform/mac/objc/crash_reporting.h");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/crash_reporting.m");

    // We need to tell `Clang` to build with a specific framework path. This is represented within
    // Clang by the `-F` flag, which is not supported directly in the `cc::Build` API, so we
    // directly pass the flag and its value instead.
    cc::Build::new()
        .file("src/platform/mac/objc/crash_reporting.m")
        .flag(format!("-F{sentry_framework_path}").as_str())
        .compile("warp_sentry_objc");
}

#[cfg(unix)]
fn get_xcode_toolchain() -> PathBuf {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let mut output = Command::new("xcode-select")
        .arg("-p")
        .output()
        .expect("Could not run xcode-select");
    if !output.status.success() {
        panic!(
            "`xcode-select -p` failed:\n\n--- stdout\n{}\n\n--- stderr\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    // Trim trailing whitespace.
    while output
        .stdout
        .last()
        .is_some_and(|b| b.is_ascii_whitespace())
    {
        output.stdout.pop();
    }

    PathBuf::from(OsString::from_vec(output.stdout)).join("Toolchains/XcodeDefault.xctoolchain")
}

#[cfg(not(unix))]
fn get_xcode_toolchain() -> PathBuf {
    panic!("get_xcode_toolchain is only supported on macOS")
}

fn copy_async_assets() {
    // Mnemos IDE: `assets/async` may not exist (its upstream art is gone). Cargo reruns a build
    // script on every build when a rerun-if-changed path is missing, so watch it only if present.
    if Path::new(ASSETS_DIR).join(ASYNC_ASSETS_DIR).is_dir() {
        println!("cargo:rerun-if-changed=assets/async");
    }
    println!("cargo:rerun-if-env-changed=ASSET_TARGET_DIR");
    let Ok(out_dir_str) = env::var("ASSET_TARGET_DIR") else {
        // Don't build assets if no target dir specified.
        return;
    };
    let out_dir = Path::new(&out_dir_str);

    let remote_asset_subdirs = &[ASYNC_ASSETS_DIR, REMOTE_ASSETS_DIR];
    for remote_asset_subdir in remote_asset_subdirs {
        let asset_dir = Path::new(ASSETS_DIR).join(remote_asset_subdir);
        // Mnemos IDE: skip a missing asset dir instead of panicking in WalkDir.
        if !asset_dir.is_dir() {
            continue;
        }

        for asset in WalkDir::new(&asset_dir) {
            let asset = asset.expect("access error");
            let asset_path = asset.path();
            if asset_path.is_file() {
                let contents = fs::read(asset_path).expect("could not read file");

                let mut hasher = sha2::Sha256::new();
                hasher.update(&contents);
                let hash: [u8; 32] = hasher.finalize().into();
                let new_relative_path = mnemos_util::assets::hashed_asset_path(
                    asset_path
                        .strip_prefix(&asset_dir)
                        .expect("asset in unexpected location"),
                    &hash,
                );
                let new_path = out_dir.join(new_relative_path);

                fs::create_dir_all(new_path.parent().unwrap())
                    .expect("failed to create directories");
                fs::write(new_path, contents).expect("failed to copy file");
            }
        }
    }
}

/// Copies the DLLs needed to run Warp on Windows.
///
/// They are organized as follows:
/// - `conpty.dll`
/// - `{platform}/OpenConsole.exe` (ex: `x64/OpenConsole.exe`)
/// - `dxcompiler.dll` (ex: `dxcompiler.dll`)
/// - `dxil.dll` (ex: `dxil.dll`)
fn copy_windows_assets(target_dir: &Path) {
    println!("cargo:rerun-if-changed=assets/windows");

    let target_arch = match std::env::var("CARGO_CFG_TARGET_ARCH")
        .expect("Target arhcitecture expected")
        .as_str()
    {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => {
            panic!("Unsupported architecture");
        }
    };

    // This directory is architecture-specific.
    let windows_asset_dir = Path::new(ASSETS_DIR)
        .join(WINDOWS_ASSETS_DIR)
        .join(target_arch);

    // Copy conpty.dll into target directory.
    fs::copy(
        windows_asset_dir.join(CONPTY_DLL_FILE),
        target_dir.join(CONPTY_DLL_FILE),
    )
    .unwrap_or_else(|err| {
        panic!("Could not copy conpty.dll from {windows_asset_dir:?} to {target_dir:?}: {err:#}")
    });

    // Copy the DXC DLLs into the target directory.
    for dxc_file in [DXCOMPILER_DLL_FILE, DXIL_DLL_FILE] {
        fs::copy(
            windows_asset_dir.join(dxc_file),
            target_dir.join(dxc_file),
        )
        .unwrap_or_else(|err| {
            panic!("Could not copy {dxc_file} from {windows_asset_dir:?} to {target_dir:?}: {err:#}")
        });
    }

    // Copy OpenConsole.exe into {target_directory}/{arch}.
    let old_open_console_exe = windows_asset_dir.join(OPEN_CONSOLE_EXE_FILE);
    let new_platform_dir = target_dir.join(target_arch);
    let new_open_console_exe = new_platform_dir.join(OPEN_CONSOLE_EXE_FILE);
    fs::create_dir_all(&new_platform_dir).expect("Could not create new platform directory");
    fs::copy(old_open_console_exe, new_open_console_exe)
        .expect("Could not copy platform OpenConsole.exe");
}

#[cfg(windows)]
fn embed_resource_file(target_dir: &Path) {
    use std::io::Write;

    // Build scripts do not track their own env reads, so without this a release cut minutes after
    // the last one reuses the cached script output and stamps the previous version into the exe.
    println!("cargo:rerun-if-env-changed=GIT_RELEASE_TAG");
    println!("cargo:rerun-if-env-changed=MNEMOS_APP_NAME");
    println!("cargo:rerun-if-env-changed=CARGO_BIN_NAME");

    let version = env::var("GIT_RELEASE_TAG").unwrap_or("v0".to_owned());
    // Shown by Task Manager (FileDescription) and file Properties. Mnemos
    // branding; the env override is kept for channel-specific bundle names.
    let app_name = env::var("MNEMOS_APP_NAME")
        .or_else(|_| env::var("WARP_APP_NAME"))
        .unwrap_or("Mnemos".to_owned());
    let bin_name = env::var("CARGO_BIN_NAME").unwrap_or("local".to_owned());

    let preferred_icon_path = Path::new("channels")
        .join(&bin_name)
        .join("icon")
        .join("no-padding")
        .join("icon.ico");

    let icon_path = if preferred_icon_path.exists() {
        preferred_icon_path
    } else {
        // Fall back to the stable channel icon — channels other than stable were dropped, but
        // helper bins (e.g. generate_settings_schema) still trigger this build script and don't
        // have their own icon directory. Skip resource embedding entirely if even stable is gone.
        let fallback = Path::new("channels")
            .join("stable")
            .join("icon")
            .join("no-padding")
            .join("icon.ico");
        if !fallback.exists() {
            println!(
                "cargo:warning=No channel icon found for bin '{bin_name}'; skipping Windows resource embed"
            );
            return;
        }
        fallback
    };

    println!("cargo:rerun-if-changed={}", icon_path.display());
    fs::copy(&icon_path, target_dir.join("icon.ico"))
        .unwrap_or_else(|err| panic!("Could not copy icon from {}: {err:#}", icon_path.display()));

    let resource_file_path = target_dir.join("resource.rc");
    let mut rcfile = fs::File::create(&resource_file_path).unwrap();
    write!(
        rcfile,
        r#"
#pragma code_page(65001)
#include <winres.h>
#define IDI_ICON 0x101

IDI_ICON ICON "icon.ico"
VS_VERSION_INFO VERSIONINFO
FILEVERSION     1,0,0,0
PRODUCTVERSION  1,0,0,0
FILEFLAGSMASK   VS_FFI_FILEFLAGSMASK
FILEFLAGS       0
FILEOS          VOS__WINDOWS32
FILETYPE        VFT_APP
FILESUBTYPE     VFT2_UNKNOWN
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "040904E4"
        BEGIN
            VALUE "CompanyName",      "TwinCurrent ApS\0"
            VALUE "FileDescription",  "{app_name}\0"
            VALUE "FileVersion",      "{version}\0"
            VALUE "LegalCopyright",   "© 2026 TwinCurrent ApS. AGPL-3.0; based on Warp by Denver Technologies, Inc\0"
            VALUE "InternalName",     "mnemos\0"
            VALUE "OriginalFilename", "mnemos.exe\0"
            VALUE "ProductName",      "Mnemos\0"
            VALUE "ProductVersion",   "{version}\0"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x409, 1252
    END
END
"#,
    )
    .unwrap();
    drop(rcfile);

    // Obtain MSVC environment so that the rc compiler can find the right headers.
    // https://github.com/nabijaczleweli/rust-embed-resource/issues/11#issuecomment-603655972
    let target = env::var("TARGET").unwrap();
    if let Some(tool) = cc::windows_registry::find_tool(target.as_str(), "cl.exe") {
        for (key, value) in tool.env() {
            env::set_var(key, value);
        }
    }
    embed_resource::compile(resource_file_path, embed_resource::NONE)
        .manifest_required()
        .unwrap_or_else(|err| panic!("Unable to embed resource file: {err:#}"));
}
