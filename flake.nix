{
  description = "Configurable jj subcommand for GitHub PR workflows";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      crane,
      nixpkgs,
    }:
    let
      inherit (nixpkgs) lib;

      forAllSystems =
        f:
        lib.genAttrs [ "aarch64-darwin" "aarch64-linux" "x86_64-linux" ] (
          system:
          f (
            import ./nix/pkgs {
              pkgs = nixpkgs.legacyPackages.${system};
              inherit crane;
            }
          )
        );
    in
    {
      packages = forAllSystems (
        {
          jj-gh,
          dev,
          gen-docs,
          gen-manpage,
          ...
        }:
        {
          default = jj-gh;
          inherit
            dev
            gen-docs
            gen-manpage
            ;
        }
      );

      apps = forAllSystems (
        {
          jj-gh,
          update-docs,
          release-app,
          fetch-schema-app,
          ...
        }:
        let
          mkApp = drv: {
            type = "app";
            program = lib.getExe drv;
          };
        in
        {
          default = mkApp jj-gh;
          docs = mkApp update-docs;
          release = mkApp release-app;
          fetch-schema = mkApp fetch-schema-app;
        }
      );

      formatter = forAllSystems ({ treefmt, ... }: treefmt);

      devShells = forAllSystems (
        {
          pkgs,
          craneLib,
          jj-gh,
          treefmt,
          ...
        }:
        {
          default = craneLib.devShell {
            inputsFrom = [ jj-gh ];
            packages = [
              pkgs.cargo-nextest
              pkgs.cargo-semver-checks
              pkgs.cargo-machete
              pkgs.ast-grep
              pkgs.jujutsu
              pkgs.rust-analyzer
              treefmt
            ];
          };
        }
      );

      checks = forAllSystems (
        {
          pkgs,
          craneLib,
          commonArgs,
          cargoArtifacts,
          treefmt,
          jj-gh,
          update-docs,
          ...
        }:
        import ./nix/checks {
          inherit
            self
            pkgs
            craneLib
            commonArgs
            cargoArtifacts
            treefmt
            jj-gh
            update-docs
            ;
        }
      );

      overlays.default = final: _prev: {
        jj-gh = self.packages.${final.stdenv.hostPlatform.system}.default;
      };

      homeManagerModules = rec {
        default = jj-gh;
        jj-gh = import ./nix/hm-module.nix self;
      };
    };
}
