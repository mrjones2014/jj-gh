{
  craneLib,
  commonArgs,
  cargoArtifacts,
}:
craneLib.cargoClippy (
  commonArgs
  // {
    inherit cargoArtifacts;
    cargoClippyExtraArgs = "--workspace --all-targets -- -D warnings";
  }
)
