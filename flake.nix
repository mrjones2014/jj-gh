{
  description = "Configurable jj subcommand for GitHub PR workflows";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      crane,
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

        src = craneLib.cleanCargoSource ./.;
        commonArgs = {
          inherit src;
          strictDeps = true;
          buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.libiconv
          ];
        };
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        jj-gh = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
          }
        );
      in
      {
        packages.default = jj-gh;
        apps.default = flake-utils.lib.mkApp { drv = jj-gh; };
        devShells.default = craneLib.devShell {
          inputsFrom = [ jj-gh ];
          packages = [
            pkgs.actionlint
            pkgs.cargo-nextest
            pkgs.jujutsu
            pkgs.rust-analyzer
            pkgs.taplo
            pkgs.yamlfmt
          ];
        };
        checks =
          let
            sourceTree = pkgs.lib.cleanSource ./.;
          in
          {
            inherit jj-gh;
            clippy = craneLib.cargoClippy (
              commonArgs
              // {
                inherit cargoArtifacts;
                cargoClippyExtraArgs = "--all-targets -- -D warnings";
              }
            );
            fmt = craneLib.cargoFmt { inherit src; };
            nextest = craneLib.cargoNextest (
              commonArgs
              // {
                inherit cargoArtifacts;
                partitions = 1;
                partitionType = "count";
              }
            );
            yamlfmt = pkgs.runCommand "yamlfmt-check" {
              nativeBuildInputs = [ pkgs.yamlfmt ];
            } ''
              cd ${sourceTree}
              yamlfmt -lint .
              touch $out
            '';
            actionlint = pkgs.runCommand "actionlint-check" {
              nativeBuildInputs = [ pkgs.actionlint ];
            } ''
              actionlint ${sourceTree}/.github/workflows/*.yml
              touch $out
            '';
            taplo = pkgs.runCommand "taplo-check" {
              nativeBuildInputs = [ pkgs.taplo ];
            } ''
              cd ${sourceTree}
              taplo fmt --check
              touch $out
            '';
          };
      }
    )
    // {
      overlays.default = final: _prev: {
        jj-gh = self.packages.${final.stdenv.hostPlatform.system}.default;
      };
    };
}
