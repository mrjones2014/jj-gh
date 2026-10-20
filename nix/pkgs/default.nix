{
  pkgs,
  crane,
}:
let
  inherit (pkgs) lib;

  # Crane only builds the CI checks and the dev tools, because its cached
  # dependency artifacts speed up repeated builds. The `jj-gh` package itself
  # uses nixpkgs' `buildRustPackage`.
  craneLib = crane.mkLib pkgs;

  src =
    let
      root = ../..;
    in
    lib.fileset.toSource {
      inherit root;
      fileset = lib.fileset.unions [
        (craneLib.fileset.commonCargoSources root)
        (lib.fileset.fileFilter (f: f.hasExt "gql") root)
        (lib.fileset.fileFilter (f: f.hasExt "graphql") root)
      ];
    };

  commonArgs = {
    inherit src;
    strictDeps = true;
    buildInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.openssl ];
    nativeBuildInputs = lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.pkg-config ];
  };

  # Explicitly workspace-wide. The root manifest has a `[package]`
  # section, so a bare `cargo build` would select only `jj-gh` and the
  # artifacts would miss deps that the devtool packages need. A superset
  # is safe here: `--workspace` resolves the same features as every
  # narrower selection used below, it just covers more crates.
  cargoArtifacts = craneLib.buildDepsOnly (
    commonArgs // { cargoExtraArgs = "--locked --workspace"; }
  );

  treefmt = import ../treefmt.nix { inherit pkgs; };

  callPackage = lib.callPackageWith {
    inherit
      pkgs
      craneLib
      commonArgs
      cargoArtifacts
      treefmt
      ;
  };

  dev = callPackage ./jj-gh-dev.nix { };
  fetch-schema-app = callPackage ./fetch-schema.nix { };
  gen-docs = callPackage ./gen-docs.nix { };
  gen-manpage = callPackage ./gen-manpage.nix { };
  jj-gh = callPackage ./jj-gh.nix { };
  release-app = callPackage ./release.nix { };
  update-docs = callPackage ./update-docs.nix { inherit gen-docs; };
in
{
  inherit
    pkgs
    craneLib
    commonArgs
    cargoArtifacts
    treefmt
    dev
    fetch-schema-app
    gen-docs
    gen-manpage
    jj-gh
    release-app
    update-docs
    ;
}
