{
  craneLib,
  commonArgs,
  cargoArtifacts,
}:
craneLib.buildPackage (
  commonArgs
  // {
    inherit cargoArtifacts;
    cargoExtraArgs = "--locked --package jj-gh";
    CARGO_PROFILE = "dev";
    doCheck = false;
  }
)
