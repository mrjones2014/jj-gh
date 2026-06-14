{
  craneLib,
  commonArgs,
  cargoArtifacts,
}:
craneLib.buildPackage (
  commonArgs
  // {
    inherit cargoArtifacts;
    cargoExtraArgs = "--package jj-gh";
    CARGO_PROFILE = "dev";
    doCheck = false;
  }
)
