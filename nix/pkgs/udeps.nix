{
  pkgs,
  craneLibNightly,
  commonArgs,
  cargoArtifactsNightly,
}:
craneLibNightly.mkCargoDerivation (
  commonArgs
  // {
    cargoArtifacts = cargoArtifactsNightly;
    pnameSuffix = "-udeps";
    buildPhaseCargoCommand = "cargo udeps --workspace --all-targets --locked";
    nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.cargo-udeps ];
  }
)
