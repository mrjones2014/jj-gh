{ pkgs, commonArgs }:
pkgs.runCommand "cargo-machete-check" { nativeBuildInputs = [ pkgs.cargo-machete ]; } ''
  cd ${commonArgs.src}
  cargo-machete
  touch $out
''
