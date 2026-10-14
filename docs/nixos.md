# NixOS

The flake provides a source package and dev shell for `x86_64-linux`. Enable flakes:

```nix
nix.settings.experimental-features = [ "nix-command" "flakes" ];
```

## Run

```sh
nix run github:Twincurrent/mnemos-ide
```

The first build compiles locally; no project binary cache is published. Nix manages updates.

## Install with a flake

Add to your configuration's `flake.nix`:

```nix
inputs.mnemos.url = "github:Twincurrent/mnemos-ide";
```

Pass `inputs` via `specialArgs = { inherit inputs; };`, then add to a NixOS module:

```nix
{ inputs, pkgs, ... }: {
  environment.systemPackages = [
    inputs.mnemos.packages.${pkgs.stdenv.hostPlatform.system}.default
  ];
}
```

For Home Manager, use `extraSpecialArgs` and `home.packages` instead. Update the `mnemos`
input in your lock file and rebuild to upgrade.

Installation includes icons, a desktop entry, and the `mnemos://` sign-in URL handler.
GPU drivers come from the host's NixOS graphics configuration.

## Development

```sh
nix build       # release package at ./result
nix develop -c cargo run --bin mnemos --features gui
nix fmt
nix flake check # package build and installation checks
```

`flake.lock` pins Nix inputs; `rust-toolchain.toml` pins Rust. Debug builds use isolated `dev`
data; the packaged application uses normal installed-app data.

After changing `Cargo.lock`, set `cargoHash` in `nix/package.nix` to `lib.fakeHash`, build, and
replace it with the hash Nix reports. Rebuild to verify.
