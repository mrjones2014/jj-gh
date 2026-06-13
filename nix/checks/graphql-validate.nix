{ pkgs, commonArgs }:
pkgs.runCommand "graphql-validate"
  {
    nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
      (pkgs.python3.withPackages (ps: [ ps.graphql-core ]))
    ];
  }
  ''
    shopt -s nullglob
    docs=(${../../src/gh/queries}/*.gql)
    if (( ''${#docs[@]} == 0 )); then
      echo "no .gql files found under src/gh" >&2
      exit 1
    fi
    python3 ${../../tools/graphql-validate.py} ${../../src/gh/github.graphql} "''${docs[@]}"
    touch $out
  ''
