{ pkgs }:
# TODO remove when PR merges and nixpkgs updated: https://github.com/release-plz/release-plz/pull/2857
pkgs.release-plz.overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [
    (pkgs.fetchpatch {
      name = "walk-all-branches-2857.patch";
      url = "https://github.com/release-plz/release-plz/commit/0bf9940e70958cb3777dae063bfda2db77d40502.patch";
      hash = "sha256-ii1SQ64Wg38sbboafTReOJji8brBYOBfaDjKHbe7SoI=";
    })
  ];
})
