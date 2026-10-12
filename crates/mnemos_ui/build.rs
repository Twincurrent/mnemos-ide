// We can use `std::process:Command` here because this is invoked within a build script,
// _not_ within the Mnemos binary (where it could cause a terminal to temporarily flash on
// Windows).
#![allow(clippy::disallowed_types)]

use std::{
    env,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use cfg_aliases::cfg_aliases;

fn main() {
    cfg_aliases! {
        macos: { target_os = "macos" },
        // We use winit on all platforms other than mac, where we have a custom
        // AppKit-based platform implementation.
        winit: { not(macos) },
        // We use wgpu for rendering on all platforms where we use winit, but
        // we can also use it on macOS, if enabled.
        wgpu: { any(winit, feature = "experimental-wgpu-renderer") },
        native: { not(target_family = "wasm") },
    }

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        bindgen_shader_types();
        compile_metal_shaders();
        compile_objc_lib();
    }
}

fn bindgen_shader_types() {
    let header_path = "src/platform/mac/rendering/metal/shaders/shader_types.h";
    println!("cargo:rerun-if-changed={header_path}");
    let bindings = bindgen::Builder::default()
        .header(header_path)
        .allowlist_type("vector_float2")
        .allowlist_type("Uniforms")
        .allowlist_type("PerRectUniforms")
        .allowlist_type("PerGlyphUniforms")
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        // Disable intrinsic headers that define types containing 16-bit floats (`_Float16`,
        // `__m512h`, etc.) via preprocessor directive. `bindgen` doesn't know how to process
        // these types and panics at compile time. The types aren't used by our shader headers,
        // so suppressing the declarations is safe.
        //
        // - Xcode 15+: avx512fp16intrin.h, avx512vlfp16intrin.h
        // - Xcode 26+ (clang 21): amxavx512intrin.h, avx10_2convertintrin.h,
        //   avx10_2_512convertintrin.h
        //
        // TODO(charlespierce): Remove once https://github.com/rust-lang/rust-bindgen/issues/2500
        // is resolved.
        .clang_args([
            "-D__AVX512VLFP16INTRIN_H",
            "-D__AVX512FP16INTRIN_H",
            "-D__AMX_AVX512INTRIN_H",
            "-D__AVX10_2CONVERTINTRIN_H",
            "-D__AVX10_2_512CONVERTINTRIN_H",
            "-D__AVX10_2_512MINMAXINTRIN_H",
            "-D__AVX10_2_512NIINTRIN_H",
            "-D__AVX10_2_512SATCVTINTRIN_H",
        ])
        .generate()
        .unwrap_or_else(|_| panic!("unable to generate bindings for {header_path}"));

    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("shader_types.rs"))
        .expect("Couldn't write shader type bindings!");
}

fn compile_metal_shaders() {
    let header_path = "src/platform/mac/rendering/metal/shaders/shader_types.h";
    let metal_path = "src/platform/mac/rendering/metal/shaders/shaders.metal";
    let out_path = PathBuf::from(env::var("OUT_DIR").unwrap());

    let air_path = out_path.join("shaders.air");
    let air_path = air_path.to_str().unwrap();

    let lib_path = out_path.join("shaders.metallib");
    let lib_path = lib_path.to_str().unwrap();

    println!("cargo:rerun-if-changed={header_path}");
    println!("cargo:rerun-if-changed={metal_path}");

    let mut compile_args = vec!["-sdk", "macosx", "metal", "-c", metal_path, "-o", air_path];
    if cfg!(feature = "enable-metal-frame-capture") {
        compile_args.push("-frecord-sources");
        compile_args.push("-gline-tables-only");
    }
    // Plain `xcrun` first. When it works, nothing below changes the build.
    let mut shim_dir = None;
    let mut result = run_xcrun(None, &compile_args).expect("error compiling metal shaders to .air");
    let mut hint = "";
    if !result.status.success() && metal_toolchain_missing(&result.stderr) {
        match metal_toolchain_shim_dir() {
            Some(dir) => {
                println!(
                    "cargo:warning=xcrun metal is unavailable (Metal Toolchain component missing); \
                     compiling shaders with the cached toolchain via script/macos/metal_toolchain \
                     ({}). Permanent fix: xcodebuild -downloadComponent MetalToolchain",
                    dir.display()
                );
                result = run_xcrun(Some(&dir), &compile_args)
                    .expect("error compiling metal shaders to .air");
                shim_dir = Some(dir);
            }
            None => hint = "\nrun script/macos/metal_toolchain for details",
        }
    }
    assert!(
        result.status.success(),
        "error compiling metal shaders to .air; {}{hint}",
        std::str::from_utf8(&result.stderr).unwrap(),
    );

    let result = run_xcrun(
        shim_dir.as_deref(),
        &["-sdk", "macosx", "metallib", air_path, "-o", lib_path],
    )
    .expect("error compiling metal shaders to .metallib");
    assert!(
        result.status.success(),
        "error compling metal shaders to .metallib; {}",
        std::str::from_utf8(&result.stderr).unwrap(),
    );
}

/// Runs `xcrun` with `args`: the one on PATH, or the metal_toolchain shim in `shim_dir` with
/// that directory prepended to the child's PATH.
fn run_xcrun(shim_dir: Option<&Path>, args: &[&str]) -> std::io::Result<Output> {
    let Some(dir) = shim_dir else {
        return Command::new("xcrun").args(args).output();
    };
    let mut paths = vec![dir.to_path_buf()];
    if let Some(path) = env::var_os("PATH") {
        paths.extend(env::split_paths(&path));
    }
    let mut command = Command::new(dir.join("xcrun"));
    if let Ok(path) = env::join_paths(paths) {
        command.env("PATH", path);
    }
    command.args(args).output()
}

/// Whether a failed `.air` compile is Xcode's "missing Metal Toolchain" stub (or a stale shim
/// on PATH) rather than a shader error. Only ever true on a macOS host.
fn metal_toolchain_missing(stderr: &[u8]) -> bool {
    if !cfg!(target_os = "macos") {
        return false;
    }
    if String::from_utf8_lossy(stderr).contains("missing Metal Toolchain") {
        return true;
    }
    // A stale shim from before a reboot fails with an exec error instead. Tell that apart from a
    // shader error by asking for the compiler version: if the real xcrun or the one on PATH can
    // run the compiler, the toolchain is fine and the shaders are at fault.
    let metal_runs = |xcrun: &str| {
        Command::new(xcrun)
            .args(["-sdk", "macosx", "metal", "-v"])
            .output()
            .is_ok_and(|output| output.status.success())
    };
    !metal_runs("/usr/bin/xcrun") && !metal_runs("xcrun")
}

/// Runs script/macos/metal_toolchain, which mounts the locally cached MetalToolchain asset and
/// generates an `xcrun` shim for it. Returns the shim directory, or `None` if the script is
/// absent or could not provide one. Its diagnostics are passed on as cargo warnings.
fn metal_toolchain_shim_dir() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR")?);
    let script = manifest_dir.join("../../script/macos/metal_toolchain");
    if !script.is_file() {
        return None;
    }
    let output = Command::new("/bin/bash").arg(&script).output().ok()?;
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        println!("cargo:warning=metal_toolchain: {line}");
    }
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let dir = PathBuf::from(stdout.trim());
    (!dir.as_os_str().is_empty() && dir.join("xcrun").is_file()).then_some(dir)
}

fn compile_objc_lib() {
    println!("cargo:rustc-link-lib=framework=UserNotifications");
    println!("cargo:rustc-link-lib=framework=Carbon");
    println!("cargo:rustc-link-lib=framework=SystemConfiguration");
    println!("cargo:rustc-link-lib=framework=UniformTypeIdentifiers");
    println!("cargo:rustc-link-lib=framework=AVFoundation");
    println!("cargo:rustc-link-lib=framework=ServiceManagement");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/app.h");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/app.m");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/keycode.m");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/host_view.m");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/host_view.h");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/hotkey.h");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/hotkey.m");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/menus.h");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/menus.m");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/notifications/notifications.h");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/notifications/notifications.m");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/window.m");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/window_blur.m");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/window_blur.h");
    // Referenced from https://github.com/tonymillion/Reachability
    println!("cargo:rerun-if-changed=src/platform/mac/objc/reachability.h");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/reachability.m");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/alert.m");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/alert.h");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/fullscreen_queue.h");
    println!("cargo:rerun-if-changed=src/platform/mac/objc/fullscreen_queue.m");

    // Link against the clang_rt library so that the @available keyword
    // doesn't produce linker errors.
    //
    // See: https://github.com/alexcrichton/curl-rust/issues/279
    if let Some(path) = macos_link_search_path() {
        println!("cargo:rustc-link-lib=clang_rt.osx");
        println!("cargo:rustc-link-search={path}");
    }

    cc::Build::new()
        .file("src/platform/mac/objc/app.m")
        .file("src/platform/mac/objc/host_view.m")
        .file("src/platform/mac/objc/hotkey.m")
        .file("src/platform/mac/objc/reachability.m")
        .file("src/platform/mac/objc/keycode.m")
        .file("src/platform/mac/objc/menus.m")
        .file("src/platform/mac/objc/notifications/notifications.m")
        .file("src/platform/mac/objc/window.m")
        .file("src/platform/mac/objc/fullscreen_queue.m")
        .file("src/platform/mac/objc/window_blur.m")
        .file("src/platform/mac/objc/alert.m")
        .compile("mnemos_objc");
}

/// Determine the path containing the macOS standard libraries by querying
/// clang's library search paths.
fn macos_link_search_path() -> Option<String> {
    let output = Command::new("clang")
        .arg("--print-search-dirs")
        .output()
        .ok()?;
    if !output.status.success() {
        println!(
            "failed to run 'clang --print-search-dirs', continuing without a link search path"
        );
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if line.contains("libraries: =") {
            let path = line.split('=').nth(1)?;
            return Some(format!("{path}/lib/darwin"));
        }
    }

    println!("failed to determine link search path, continuing without it");
    None
}
