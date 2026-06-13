{
  craneLib,
  commonArgs,
  cargoArtifacts,
}:
craneLib.cargoNextest (
  commonArgs
  // {
    inherit cargoArtifacts;
    cargoNextestExtraArgs = "--workspace";
    partitions = 1;
    partitionType = "count";
  }
)
