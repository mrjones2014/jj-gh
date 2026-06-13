{
  craneLib,
  commonArgs,
  cargoArtifacts,
}:
craneLib.buildPackage (
  commonArgs
  // {
    inherit cargoArtifacts;
    pname = "gen-docs";
    cargoExtraArgs = "--package gen-docs";
    doCheck = false;
  }
)
