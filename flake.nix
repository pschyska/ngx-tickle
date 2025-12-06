{
  description = "ngx-tickle";

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

      outputsBuilder =
        channels:
        let
          pkgs = channels.nixpkgs;
          toolchain = pkgs.rust-bin.stable.latest.default;
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
              openssl
              zlib
            ];
            language.c.libraries = [
              pcre2
              openssl
              zlib
              libclang
            ];
            env = [
              {
                name = "PKG_CONFIG_PATH";
                prefix = "$DEVSHELL_DIR/share/pkgconfig";
              }
            ];
            language.rust.enableDefaultToolchain = false;
            packages = [ toolchain ];
          };
        };
    };
}
