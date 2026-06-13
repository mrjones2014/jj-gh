{ craneLib, commonArgs }:
let
  cargoArtifacts = craneLib.buildDepsOnly (
    commonArgs
    // {
      pname = "print-config-schema-deps";
      cargoExtraArgs = "--package print-config-schema";
    }
  );
in
craneLib.buildPackage (
  commonArgs
  // {
    inherit cargoArtifacts;
    pname = "print-config-schema";
    cargoExtraArgs = "--package print-config-schema";
    doCheck = false;
  }
)
