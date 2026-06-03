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

          ff = pkgs.writeShellApplication {
            name = "ff";

            runtimeInputs = with pkgs; [
              toolchain
              gnugrep
              findutils
            ];
            text = ''
              (
                cd "$PRJ_ROOT"
                printf "# cargo fix\n"
                cargo fix --workspace --all-targets --allow-dirty
                printf "\n# cargo clippy --fix\n"
                cargo clippy --workspace --all-targets --fix --allow-dirty
                printf "\n# rustfmt\n"
                git ls-files | grep '\.rs$' | xargs -r rustfmt --edition 2024
              )
            '';
          };
          # quarto: --syntax-highlighting
          pandoc = pkgs.stdenv.mkDerivation {
            pname = "p-pandoc";
            version = "3.9.0.2";
            src = pkgs.fetchurl {
              url = "https://github.com/jgm/pandoc/releases/download/3.9.0.2/pandoc-3.9.0.2-linux-amd64.tar.gz";
              sha256 = "sha256-ppq/q6vailaWmiVLCflVOnvond7ADU4P6f1YXXGmdQg=";
            };

            dontConfigure = true;
            dontBuild = true;

            installPhase = ''
              runHook preInstall
              install -Dm755 -t $out/bin bin/pandoc bin/pandoc-lua bin/pandoc-server
              install -Dm644 -t $out/share/man/man1 share/man/man1/*.1.gz
              runHook postInstall
            '';

            meta = {
              homepage = "https://pandoc.org";
              description = "Universal markup converter (upstream prebuilt static binary)";
              mainProgram = "pandoc";
              license = pkgs.lib.licenses.gpl2Plus;
              platforms = [ "x86_64-linux" ];
            };
          };
          pythonPackages =  ps: with ps; [
                  jupyter
                  ipykernel
                  nbclient
                  nbformat
                  pandas
                  tabulate
                  polars
                  altair
                  vl-convert-python
                ];
          python = pkgs.python3.withPackages pythonPackages;
          quarto = (
            pkgs.quarto.override {
              inherit pandoc;
              extraPythonPackages = pythonPackages;
            }
          );
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
            packages = [
              toolchain
              gnumake
              ff

              gdb
              # benchmarks
              heaptrack
              perf
              wrk
              wrk2
              xan
              quarto
              python
            ];
          };
        };
    };
}
