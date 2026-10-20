{
  self,
  pkgs,
  craneLib,
  commonArgs,
  cargoArtifacts,
  treefmt,
  update-docs,
  jj-gh,
}:
let
  callPackage = pkgs.lib.callPackageWith {
    inherit
      self
      pkgs
      craneLib
      commonArgs
      cargoArtifacts
      treefmt
      update-docs
      jj-gh
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
