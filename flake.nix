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
  };

  outputs =
    {
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      crane,
      treefmt-nix,
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

        src = craneLib.cleanCargoSource ./.;
        commonArgs = {
          inherit src;
          strictDeps = true;
          buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.libiconv
          ];
        };
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;
        cargoArtifactsNightly = craneLibNightly.buildDepsOnly commonArgs;
        jj-gh = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;
          }
        );
        treefmtEval = treefmt-nix.lib.evalModule pkgs (import ./nix/treefmt.nix { inherit rustToolchain; });
      in
      {
        packages.default = jj-gh;
        apps.default = flake-utils.lib.mkApp { drv = jj-gh; };
        formatter = treefmtEval.config.build.wrapper;
        devShells.default = craneLib.devShell {
          inputsFrom = [ jj-gh ];
          packages = [
            pkgs.cargo-nextest
            pkgs.cargo-udeps
            pkgs.jujutsu
            pkgs.rust-analyzer
            treefmtEval.config.build.wrapper
          ];
        };
        checks = {
          inherit jj-gh;
          clippy = craneLib.cargoClippy (
            commonArgs
            // {
              inherit cargoArtifacts;
              cargoClippyExtraArgs = "--all-targets -- -D warnings";
            }
          );
          nextest = craneLib.cargoNextest (
            commonArgs
            // {
              inherit cargoArtifacts;
              partitions = 1;
              partitionType = "count";
            }
          );
          udeps = craneLibNightly.mkCargoDerivation (
            commonArgs
            // {
              cargoArtifacts = cargoArtifactsNightly;
              pnameSuffix = "-udeps";
              buildPhaseCargoCommand = "cargo udeps --all-targets --locked";
              nativeBuildInputs = [ pkgs.cargo-udeps ];
            }
          );
          treefmt = treefmtEval.config.build.check self;
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
