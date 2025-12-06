{
  description = "ngx-tickle dev shell";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

    fup.url = "github:gytis-ivaskevicius/flake-utils-plus";

    devshell.url = "github:numtide/devshell";
    devshell.inputs.nixpkgs.follows = "nixpkgs";

    rust-overlay.url = "github:oxalica/rust-overlay";
    rust-overlay.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    inputs@{
      self,
      nixpkgs,
      fup,
      devshell,
      rust-overlay,
      ...
    }:
    fup.lib.mkFlake {
      inherit self inputs;
      supportedSystems = [ "x86_64-linux" ];

      sharedOverlays = [
        devshell.overlays.default
        rust-overlay.overlays.default
      ];

      channelsConfig = {
        allowUnfreePredicate = pkg: builtins.elem (nixpkgs.lib.getName pkg) [ "terraform" ];
      };
      outputsBuilder =
        channels:
        let
          pkgs = channels.nixpkgs;
          toolchain = pkgs.rust-bin.selectLatestNightlyWith (
            toolchain:
            toolchain.default.override {
              extensions = [
                "rust-analyzer"
                "rust-src"
                "miri"
                "llvm-tools-preview"
              ];
              targets = [
                "x86_64-unknown-linux-gnu"
                "x86_64-unknown-linux-musl"
              ];
            }
          );
          rustPlatform = pkgs.makeRustPlatform {
            rustc = toolchain;
            cargo = toolchain;
          };
        in
        with pkgs;
        {
          devShell = pkgs.devshell.mkShell {
            motd = "";
            imports = [
              "${devshell}/extra/language/c.nix"
              "${devshell}/extra/language/rust.nix"
            ];
            language.c.compiler = clang;
            language.c.includes = [
              pcre2
              openssl.dev
              zlib
            ];
            language.c.libraries = [
              pcre2
              openssl.dev
              zlib
              libclang
            ];
            language.rust.enableDefaultToolchain = false;
            env = [
              {
                name = "PYTHONWARNINGS";
                value = "ignore";
              }
            ];
            packages = [
              lldb
              clang-tools

              pkg-config
              toolchain
              heaptrack
              (cargo-llvm-cov.override { inherit rustPlatform; })
            ];
          };
        };
    };
}
