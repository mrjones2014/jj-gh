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
    cargoExtraArgs = "--locked --package gen-manpage";
    doCheck = false;
  }
)
