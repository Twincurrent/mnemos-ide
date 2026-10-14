{
  lib,
  rustPlatform,
  pkg-config,
  cmake,
  protobuf,
  brotli,
  makeWrapper,
  desktop-file-utils,
  openssl,
  freetype,
  expat,
  libgit2,
  fontconfig,
  alsa-lib,
  wayland,
  libxkbcommon,
  vulkan-loader,
  libGL,
  libx11,
  libxcb,
  libxi,
  libxcursor,
  xdg-utils,
  revision ? "",
}:
let
  lock = builtins.fromTOML (builtins.readFile ../Cargo.lock);
  gitRevision =
    name:
    lib.last (
      lib.splitString "#" (lib.findFirst (package: package.name == name) null lock.package).source
    );
  runtimeLibraries = [
    openssl
    freetype
    expat
    libgit2
    fontconfig
    alsa-lib
    wayland
    libxkbcommon
    vulkan-loader
    libGL
    libx11
    libxcb
    libxi
    libxcursor
  ];
in
rustPlatform.buildRustPackage (finalAttrs: {
  pname = "mnemos";
  version = (builtins.fromTOML (builtins.readFile ../app/Cargo.toml)).package.version;
  # Keep documentation and Nix-only edits from invalidating the Rust build.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../rust-toolchain.toml
      ../.cargo
      ../.rustfmt.toml
      ../app
      ../crates
      ../ide
      ../resources
      ../script
      ../NOTICE
      ../LICENSE-AGPL
      ../LICENSE-MIT
    ];
  };
  cargoHash = "sha256-uwe1d5g+6/4I0IMhkDunLBN05oTnE37uoVbRj5DUfMM=";

  nativeBuildInputs = [
    pkg-config
    cmake
    protobuf
    brotli
    makeWrapper
    desktop-file-utils
  ];
  buildInputs = runtimeLibraries;
  buildFeatures = [
    "gui"
    "release_bundle"
  ];
  cargoBuildFlags = [
    "--package"
    "mnemos"
    "--bin"
    "mnemos"
  ];
  # The workspace's GUI tests require a display; package checks validate installation below.
  doCheck = false;
  doInstallCheck = true;

  env = {
    MNEMOS_UPDATER = "off";
    MNEMOS_GIT_COMMIT = revision;
  };

  # These upstream build scripts read files outside their crate directories. Restore
  # the layout from the hash-checked git checkouts, using revisions from Cargo.lock.
  postPatch = ''
    cp ${finalAttrs.cargoDeps.vendorStaging}/git/${gitRevision "warp_multi_agent_api"}/apis/multi_agent/v1/*.proto \
      "$cargoDepsCopy/"
    for workflows in "$cargoDepsCopy"/source-git-*/warp-workflows-[0-9]*; do
      cp -r ${finalAttrs.cargoDeps.vendorStaging}/git/${gitRevision "warp-workflows"}/specs \
        "$(dirname "$workflows")/specs"
    done
  '';

  postInstall = ''
    install -Dm644 app/channels/stable/dev.mnemos.Mnemos.desktop \
      "$out/share/applications/dev.mnemos.Mnemos.desktop"
    substituteInPlace "$out/share/applications/dev.mnemos.Mnemos.desktop" \
      --replace-fail 'Exec=mnemos-terminal %U' "Exec=$out/bin/mnemos %U"
    for icon in app/channels/stable/icon/no-padding/*x*.png; do
      size=$(basename "$icon" .png)
      install -Dm644 "$icon" "$out/share/icons/hicolor/$size/apps/dev.mnemos.Mnemos.png"
    done
    install -Dm644 NOTICE "$out/share/doc/mnemos/NOTICE"
    install -Dm644 LICENSE-AGPL "$out/share/doc/mnemos/LICENSE-AGPL"
    install -Dm644 LICENSE-MIT "$out/share/doc/mnemos/LICENSE-MIT"

    # Load application libraries while keeping the host's GPU driver selection.
    # A suffix lets NixOS's session-provided driver/library paths take precedence.
    # The CLI detects a link named mnemos pointing at a differently named binary.
    # makeWrapper renames that binary, so use the GUI invocation name explicitly.
    wrapProgram "$out/bin/mnemos" --argv0 mnemos-terminal \
      --suffix LD_LIBRARY_PATH : "${lib.makeLibraryPath runtimeLibraries}" \
      --suffix PATH : "${lib.makeBinPath [ xdg-utils ]}"
  '';

  installCheckPhase = ''
    runHook preInstallCheck
    test -x "$out/bin/mnemos"
    test -s "$out/share/icons/hicolor/256x256/apps/dev.mnemos.Mnemos.png"
    desktop-file-validate "$out/share/applications/dev.mnemos.Mnemos.desktop"
    runHook postInstallCheck
  '';

  passthru = { inherit runtimeLibraries; };
  meta = {
    description = "GPU-rendered terminal with agent sessions and cross-machine project sync";
    homepage = "https://github.com/Twincurrent/mnemos-ide";
    license = lib.licenses.agpl3Only;
    mainProgram = "mnemos";
    platforms = [ "x86_64-linux" ];
  };
})
