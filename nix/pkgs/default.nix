{
  pkgs,
  crane,
  craneLib,
  commonArgs,
  cargoArtifacts,
  rustToolchain,
  treefmtEval,
}:
let
  nightlyToolchain = pkgs.rust-bin.selectLatestNightlyWith (toolchain: toolchain.minimal);
  craneLibNightly = (crane.mkLib pkgs).overrideToolchain nightlyToolchain;
  cargoArtifactsNightly = craneLibNightly.buildDepsOnly commonArgs;

  callPackage = pkgs.lib.callPackageWith {
    inherit
      pkgs
      craneLib
      craneLibNightly
      commonArgs
      cargoArtifacts
      cargoArtifactsNightly
      rustToolchain
      treefmtEval
      ;
  };

  gen-docs = callPackage ./gen-docs.nix { };
  gen-manpage = callPackage ./gen-manpage.nix { };
  jj-gh = callPackage ./jj-gh.nix { inherit gen-manpage; };
  print-config-schema = callPackage ./print-config-schema.nix { };
  release-plz-patched = callPackage ./release-plz-patched.nix { };
  update-docs = callPackage ./update-docs.nix { inherit gen-docs; };
in
{
  inherit
    jj-gh
    print-config-schema
    release-plz-patched
    update-docs
    ;

  fetch-schema-app = callPackage ./fetch-schema.nix { };
  release-app = callPackage ./release.nix { inherit release-plz-patched; };

  packages = {
    default = jj-gh;
    dev = callPackage ./jj-gh-dev.nix { };
    inherit gen-docs gen-manpage;
    udeps = callPackage ./udeps.nix { };
  };
}
