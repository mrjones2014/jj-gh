{
  description = "Configurable jj subcommand for GitHub PR workflows";

  inputs = {
    crane.url = "github:ipetkov/crane";
    flake-utils.url = "github:numtide/flake-utils";
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
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
      crane,
      flake-utils,
      nixpkgs,
      rust-overlay,
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

        src =
          let
            root = ./.;
          in
          pkgs.lib.fileset.toSource {
            inherit root;
            fileset = pkgs.lib.fileset.unions [
              (craneLib.fileset.commonCargoSources root)
              (pkgs.lib.fileset.fileFilter (f: f.hasExt "gql") root)
              (pkgs.lib.fileset.fileFilter (f: f.hasExt "graphql") root)
            ];
          };
        commonArgs = {
          inherit src;
          strictDeps = true;
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
        treefmtEval = treefmt-nix.lib.evalModule pkgs (import ./nix/treefmt.nix { inherit rustToolchain; });
        packageSet = import ./nix/pkgs {
          inherit
            pkgs
            crane
            craneLib
            commonArgs
            cargoArtifacts
            rustToolchain
            treefmtEval
            ;
        };
        inherit (packageSet)
          fetch-schema-app
          jj-gh
          print-config-schema
          release-app
          release-plz-patched
          update-docs
          ;
      in
      {
        inherit (packageSet) packages;
        apps = {
          default = flake-utils.lib.mkApp { drv = jj-gh; };
          docs = flake-utils.lib.mkApp {
            drv = update-docs;
            name = "update-docs";
          };
          release = flake-utils.lib.mkApp {
            drv = release-app;
            name = "release";
          };
          fetch-schema = flake-utils.lib.mkApp {
            drv = fetch-schema-app;
            name = "fetch-schema";
          };
        };
        formatter = treefmtEval.config.build.wrapper;
        devShells.default = craneLib.devShell {
          inputsFrom = [ jj-gh ];
          packages = [
            pkgs.cargo-nextest
            pkgs.cargo-semver-checks
            pkgs.cargo-udeps
            pkgs.ast-grep
            pkgs.jujutsu
            pkgs.rust-analyzer
            treefmtEval.config.build.wrapper
          ];
        };
        checks = import ./nix/checks {
          inherit
            self
            pkgs
            craneLib
            commonArgs
            cargoArtifacts
            treefmtEval
            jj-gh
            print-config-schema
            release-plz-patched
            update-docs
            ;
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
