{
  description = "Configurable jj subcommand for GitHub PR workflows";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    github-graphql-schema = {
      url = "github:octokit/graphql-schema/v15.26.1";
      flake = false;
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      crane,
      treefmt-nix,
      github-graphql-schema,
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        nightlyToolchain = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.minimal);
        craneLibNightly = (crane.mkLib pkgs).overrideToolchain nightlyToolchain;
        src =
          let
            root = ./.;
          in
          pkgs.lib.fileset.toSource {
            inherit root;
            fileset = pkgs.lib.fileset.unions [
              (craneLib.fileset.commonCargoSources root)
              (pkgs.lib.fileset.fileFilter (f: f.hasExt "gql") root)
            ];
          };
        commonArgs = {
          inherit src;
          strictDeps = true;
          postPatch = ''
            mkdir -p src/gh
            cp ${github-graphql-schema}/schema.graphql src/gh/github.graphql
          '';
          buildInputs =
            (pkgs.lib.optionals pkgs.stdenv.isDarwin [
              pkgs.libiconv
            ])
            ++ (pkgs.lib.optionals pkgs.stdenv.isLinux [
              pkgs.openssl
            ]);
          nativeBuildInputs = pkgs.lib.optionals pkgs.stdenv.isLinux [ pkgs.pkg-config ];
        };
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        cargoArtifactsNightly = craneLibNightly.buildDepsOnly commonArgs;
        jj-gh = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            cargoExtraArgs = "--package jj-gh";

            nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
              pkgs.installShellFiles
            ];

            postInstall =
              let
                jj-gh = "${pkgs.stdenv.hostPlatform.emulator pkgs.buildPackages} $out/bin/jj-gh";
              in
              pkgs.lib.optionalString (pkgs.stdenv.hostPlatform.emulatorAvailable pkgs.buildPackages) ''
                installShellCompletion --cmd jj-gh \
                  --bash <(${jj-gh} completions bash) \
                  --fish <(${jj-gh} completions fish) \
                  --zsh <(${jj-gh} completions zsh)
              '';
          }
        );
        gen-docs = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
            pname = "gen-docs";
            cargoExtraArgs = "--package gen-docs";
            doCheck = false;
          }
        );
        treefmtEval = treefmt-nix.lib.evalModule pkgs (import ./nix/treefmt.nix { inherit rustToolchain; });
        gen-docs-app = pkgs.writeShellApplication {
          name = "gen-docs";
          runtimeInputs = [
            gen-docs
            treefmtEval.config.build.wrapper
          ];
          text = ''
            gen-docs > DOCS.md
            treefmt --no-cache DOCS.md
          '';
        };
        release-app = pkgs.writeShellApplication {
          name = "release";
          runtimeInputs = [
            rustToolchain
            pkgs.release-plz
            pkgs.cargo-semver-checks
            pkgs.git
          ];
          text = ''
            ln -sfn ${github-graphql-schema}/schema.graphql src/gh/github.graphql
            release-plz release-pr --git-token "$GITHUB_TOKEN" "$@"
            release-plz release --git-token "$GITHUB_TOKEN" "$@"
          '';
        };
      in
      {
        packages = {
          default = jj-gh;
          inherit gen-docs;
          udeps = craneLibNightly.mkCargoDerivation (
            commonArgs
            // {
              cargoArtifacts = cargoArtifactsNightly;
              pnameSuffix = "-udeps";
              buildPhaseCargoCommand = "cargo udeps --workspace --all-targets --locked";
              nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.cargo-udeps ];
            }
          );
        };
        apps = {
          default = flake-utils.lib.mkApp { drv = jj-gh; };
          docs = flake-utils.lib.mkApp {
            drv = gen-docs-app;
            name = "gen-docs";
          };
          release = flake-utils.lib.mkApp {
            drv = release-app;
            name = "release";
          };
        };
        formatter = treefmtEval.config.build.wrapper;
        devShells.default = craneLib.devShell {
          inputsFrom = [ jj-gh ];
          packages = [
            pkgs.cargo-nextest
            pkgs.cargo-semver-checks
            pkgs.cargo-udeps
            pkgs.jujutsu
            pkgs.rust-analyzer
            treefmtEval.config.build.wrapper
          ];
          # this is so rust-analyzer sees it while developing; its included properly in the actual nix derivations as well
          shellHook = ''
            ln -sfn ${github-graphql-schema}/schema.graphql src/gh/github.graphql
          '';
        };
        checks = {
          inherit jj-gh;
          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
            }
          );
          nextest = craneLib.cargoNextest (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoNextestExtraArgs = "--workspace";
              partitions = 1;
              partitionType = "count";
            }
          );
          treefmt = treefmtEval.config.build.check self;
          graphql-validate =
            pkgs.runCommand "graphql-validate"
              {
                nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
                  (pkgs.python3.withPackages (ps: [ ps.graphql-core ]))
                ];
              }
              ''
                shopt -s nullglob
                docs=(${./src/gh/queries}/*.gql)
                if (( ''${#docs[@]} == 0 )); then
                  echo "no .gql files found under src/gh" >&2
                  exit 1
                fi
                python3 ${./graphql-validate.py} ${github-graphql-schema}/schema.graphql "''${docs[@]}"
                touch $out
              '';
          docs =
            pkgs.runCommand "docs-check"
              {
                nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.diffutils ];
              }
              ''
                mkdir -p $out
                cp ${./DOCS.md} "$out/expected.md"
                touch "$out/flake.nix"
                cd "$out"
                ${gen-docs-app}/bin/gen-docs
                if ! diff -u expected.md DOCS.md; then
                  echo "DOCS.md out of date; run \`nix run .#docs\`" >&2
                  exit 1
                fi
              '';
        };
      }
    )
    // {
      overlays.default = final: _prev: {
        jj-gh = self.packages.${final.stdenv.hostPlatform.system}.default;
      };
      homeManagerModules.default = import ./nix/hm-module.nix self;
      homeManagerModules.jj-gh = import ./nix/hm-module.nix self;
    };
}
