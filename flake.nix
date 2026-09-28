{
  inputs = {
    rust-overlay.url = "github:oxalica/rust-overlay";
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem
      (system:
        let
          overlays = [ (import rust-overlay) ];
          pkgs = import nixpkgs {
            inherit system overlays;
          };
          inherit (pkgs) lib;

          readToolchain = file: (builtins.fromTOML (builtins.readFile file)).toolchain;
          mergeToolchains = base: override: {
            channel    = override.channel or base.channel;
            components = lib.unique ((base.components or [ ]) ++ (override.components or [ ]));
            targets    = lib.unique ((base.targets or [ ]) ++ (override.targets or [ ]));
          };

          rootToolchain = readToolchain ./rust-toolchain.toml;
          sdkToolchain  = readToolchain ./sdk/rust-toolchain.toml;
          embeddedToolchain = readToolchain ./embedded/rust-toolchain.toml;

          # We're overwriting it here with the SDK toolchain,
          # since it currently still uses Rust Nightly
          # (see https://github.com/peeriot/myrmic/discussions/4).
          baseToolchain = mergeToolchains rootToolchain embeddedToolchain;
          toolchain = mergeToolchains baseToolchain sdkToolchain;

          rust = pkgs.rust-bin.fromRustupToolchain (toolchain // {
            # In case it is not included in the toolchain files:
            components = [ "rust-src" ];
          });
        in
        with pkgs;
        rec {
          # These are not actual dependencies
          # but tools that can help with development.
          dev_env_packages =  [
            git
            cargo-nextest
            cargo-deny
            espflash
            rust-analyzer
          ];

          devShells.default = mkShell {
            packages = [
              rust
              llvmPackages.clang-unwrapped # clang without nix wrapper
            ] ++ dev_env_packages;

            LIBCLANG_PATH = "${llvmPackages.libclang.lib}/lib";
          };
        }
    );
}
