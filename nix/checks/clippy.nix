{
  craneLib,
  commonArgs,
  cargoArtifacts,
}:
craneLib.cargoClippy (
  commonArgs
  // {
    inherit cargoArtifacts;
    cargoClippyExtraArgs = "--workspace -- -D warnings";
  }
)
