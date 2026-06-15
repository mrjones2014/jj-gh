{ pkgs }:
# TODO remove when PR merges and nixpkgs updated: https://github.com/release-plz/release-plz/pull/2857
pkgs.release-plz.overrideAttrs (old: {
  patches = (old.patches or [ ]) ++ [
    (pkgs.fetchpatch {
      name = "walk-all-branches-2857.patch";
      url = "https://github.com/release-plz/release-plz/commit/2dbce2513eea25920c0e37826d3c4eb38e25dcd4.patch";
      hash = "sha256-jcY7luLkI4YCLM93KhL1vaKkFMdPnGMzBEePJ+cHjkc=";
    })
  ];
})
