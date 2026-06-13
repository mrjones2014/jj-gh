{
  pkgs,
  craneLib,
  commonArgs,
  cargoArtifacts,
  gen-manpage,
}:
craneLib.buildPackage (
  commonArgs
  // {
    inherit cargoArtifacts;
    cargoExtraArgs = "--package jj-gh";

    nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
      pkgs.installShellFiles
    ];

    postInstall =
      let
        jj-gh = "${pkgs.stdenv.hostPlatform.emulator pkgs.buildPackages} $out/bin/jj-gh";
      in
      ''
        ${gen-manpage}/bin/gen-manpage > jj-gh.1
        installManPage jj-gh.1
      ''
      + pkgs.lib.optionalString (pkgs.stdenv.hostPlatform.emulatorAvailable pkgs.buildPackages) ''
        installShellCompletion --cmd jj-gh \
          --bash <(${jj-gh} completions bash) \
          --fish <(${jj-gh} completions fish) \
          --zsh <(${jj-gh} completions zsh)
      '';
  }
)
