{
  self,
  pkgs,
  print-config-schema,
}:
let
  hmModuleSettingsOptsJson =
    let
      mod = (import ../hm-module.nix self) {
        config = { };
        inherit (pkgs) lib;
        inherit pkgs;
      };
      names = builtins.attrNames mod.options.programs.jujutsu.gh.settings;
    in
    pkgs.writeText "hm-module-settings-opts.json" (
      builtins.toJSON (builtins.sort builtins.lessThan names)
    );
in
pkgs.runCommand "hm-module-schema"
  {
    nativeBuildInputs = [
      pkgs.jq
      pkgs.diffutils
    ];
  }
  ''
    ${print-config-schema}/bin/print-config-schema > schema.json
    jq -r '.properties | keys[]' schema.json | sort > rust-fields.txt
    jq -r '.[]' ${hmModuleSettingsOptsJson} | sort > nix-fields.txt
    if ! diff -u rust-fields.txt nix-fields.txt >&2; then
      echo "" >&2
      echo "ERROR: jj-gh Config struct fields drifted from hm-module.nix settings options." >&2
      exit 1
    fi
    touch $out
  ''
