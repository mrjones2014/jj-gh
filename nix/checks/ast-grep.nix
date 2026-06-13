{ pkgs }:
let
  root = ../..;
  src = pkgs.lib.fileset.toSource {
    inherit root;
    fileset = root;
  };
in
pkgs.runCommand "ast-grep-check" { nativeBuildInputs = [ pkgs.ast-grep ]; } ''
  cd ${src}
  ast-grep scan --error
  ast-grep test
  touch $out
''
