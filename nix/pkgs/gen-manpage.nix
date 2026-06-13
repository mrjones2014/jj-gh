{
  craneLib,
  commonArgs,
  cargoArtifacts,
}:
craneLib.buildPackage (
  commonArgs
  // {
    inherit cargoArtifacts;
    pname = "gen-manpage";
    cargoExtraArgs = "--package gen-manpage";
    doCheck = false;
  }
)
