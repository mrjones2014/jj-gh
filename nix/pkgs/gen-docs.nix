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
    cargoExtraArgs = "--locked --package gen-docs";
    doCheck = false;
  }
)
