{ pkgs, gen-docs }:
let
  inherit (pkgs) lib;
  highlightJsNix = pkgs.fetchFromGitHub {
    owner = "mrjones2014";
    repo = "highlight-js-nix";
    rev = "67aebab";
    hash = "sha256-EmeiP7TAdb7n6KOHJ8qhGzSL0TGkoVuQIkijnrAj5RQ=";
  };
  assets = pkgs.runCommand "jj-gh-book-assets" { nativeBuildInputs = [ pkgs.mdbook-mermaid ]; } ''
    mkdir -p "$out/theme"
    printf '[book]\ntitle = "assets"\n' > book.toml
    mdbook-mermaid install .
    cp mermaid.min.js mermaid-init.js "$out/"
    cp ${highlightJsNix}/highlight.js "$out/theme/highlight.js"
  '';
  cliPreprocessor = pkgs.writeShellApplication {
    name = "mdbook-jj-gh-reference";
    runtimeInputs = [
      gen-docs
      pkgs.python3
    ];
    text = ''
      exec python3 ${./reference.py} "$@"
    '';
  };
  environment = ''
    export MDBOOK_OUTPUT__HTML__ADDITIONAL_JS='["book-assets/mermaid.min.js", "book-assets/mermaid-init.js"]'
    export MDBOOK_OUTPUT__HTML__THEME='book-assets/theme'
  '';
  book = pkgs.stdenv.mkDerivation {
    pname = "jj-gh-book";
    version = "0.1.0";
    src = lib.cleanSource ./.;
    nativeBuildInputs = [
      pkgs.mdbook
      pkgs.mdbook-mermaid
      cliPreprocessor
    ];
    buildPhase = ''
      runHook preBuild
      ${environment}
      ln -s ${assets} book-assets
      mdbook build -d "$out"
      runHook postBuild
    '';
    installPhase = "true";
  };
  serve = pkgs.writeShellApplication {
    name = "serve-jj-gh-book";
    runtimeInputs = [
      pkgs.mdbook
      pkgs.mdbook-mermaid
      cliPreprocessor
    ];
    text = ''
      if [ -f ./docs/book.toml ]; then
        book_dir=./docs
      elif [ -f ./book.toml ] && [ -f ./reference.py ]; then
        book_dir=.
      else
        echo 'Run from the repository root or docs directory.' >&2
        exit 1
      fi
      if [ -e "$book_dir/book-assets" ] && [ ! -L "$book_dir/book-assets" ]; then
        echo "$book_dir/book-assets exists and is not a symlink" >&2
        exit 1
      fi
      ln -sfn ${assets} "$book_dir/book-assets"
      ${environment}
      exec mdbook serve "$book_dir" "$@"
    '';
  };
in
{
  inherit book serve;
}
