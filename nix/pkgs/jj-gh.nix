{
  pkgs,
  commonArgs,
}:
let
  cargoToml = pkgs.lib.importTOML ../../Cargo.toml;
  # `gen-manpage` links against the jj-gh lib, so building it in its own
  # derivation means a second cargo invocation that recompiles that lib from
  # scratch on every source change. Building both packages here compiles the
  # lib once. The extra binary is removed after the manpage is generated.
  cargoPackageFlags = [
    "--package"
    "jj-gh"
    "--package"
    "gen-manpage"
  ];
in
# This package uses nixpkgs' `buildRustPackage` rather than crane, because
# people who install it build it only once, so crane's separate dependency
# derivation would only make their build slower.
pkgs.rustPlatform.buildRustPackage {
  pname = cargoToml.package.name;
  inherit (cargoToml.package) version;
  inherit (commonArgs) src buildInputs;

  cargoLock.lockFile = ../../Cargo.lock;

  cargoBuildFlags = cargoPackageFlags;
  cargoTestFlags = cargoPackageFlags;

  nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
    pkgs.installShellFiles
  ];

  # Both the manpage and the completions come from running hostPlatform
  # binaries, so both need the emulator and both are skipped when none is
  # available.
  postInstall =
    let
      emulator = pkgs.stdenv.hostPlatform.emulator pkgs.buildPackages;
      jj-gh = "${emulator} $out/bin/jj-gh";
    in
    pkgs.lib.optionalString (pkgs.stdenv.hostPlatform.emulatorAvailable pkgs.buildPackages) ''
      ${emulator} $out/bin/gen-manpage > jj-gh.1
      installManPage jj-gh.1

      installShellCompletion --cmd jj-gh \
        --bash <(${jj-gh} completions bash) \
        --fish <(${jj-gh} completions fish) \
        --nushell <(${jj-gh} completions nushell) \
        --zsh <(${jj-gh} completions zsh)
    ''
    + ''
      rm -f $out/bin/gen-manpage
    '';

  meta = {
    inherit (cargoToml.package) description;
    homepage = cargoToml.package.repository;
    license = pkgs.lib.licenses.mit;
    mainProgram = "jj-gh";
  };
}
