{
  self,
  pkgs,
  craneLib,
  commonArgs,
  cargoArtifacts,
  treefmtEval,
  update-docs,
  jj-gh,
  print-config-schema,
  release-plz-patched,
}:
let
  callPackage = pkgs.lib.callPackageWith {
    inherit
      self
      pkgs
      craneLib
      commonArgs
      cargoArtifacts
      treefmtEval
      update-docs
      jj-gh
      print-config-schema
      ;
  };
  checkFiles = pkgs.lib.filterAttrs (
    name: type: type == "regular" && name != "default.nix" && pkgs.lib.hasSuffix ".nix" name
  ) (builtins.readDir ./.);
  checks = pkgs.lib.mapAttrs' (
    name: _:
    pkgs.lib.nameValuePair (pkgs.lib.removeSuffix ".nix" name) (callPackage (./. + "/${name}") { })
  ) checkFiles;
in
checks
// {
  inherit jj-gh;
}
// (pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
  # so that the patched version gets cached by CI
  inherit release-plz-patched;
})
