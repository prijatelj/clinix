# The shell definition. One file, three entry points:
#
#   nix-shell        classic; resolves sources from flake.lock below
#   nix develop      flake; Nix fetches the inputs and passes them in
#   shell            the launcher, which roots this against the GC
#
# Taking `sources`/`system`/`pkgs` as arguments is what makes that work: each
# caller supplies whatever it already resolved, so there is never a second
# nixpkgs instance and never a second lock. The defaults only fire when nobody
# supplied one — i.e. under bare `nix-shell`.
{
  # Read flake.lock. The Nix evaluator has no lock reader, so nix-shell needs
  # this translation written in the language. `nix develop` never evaluates it:
  # Nix has already fetched the inputs and passes `sources` in.
  #
  # `locked.narHash` is the NAR hash of the unpacked tree, which is exactly
  # what fetchTarball's `sha256` verifies; for git, `rev` is already the
  # content address. Same translation NixOS/flake-compat performs.
  #
  # If a second file ever needs `sources` — say a default.nix that packages
  # something — lift this block into its own lock.nix and import it from both.
  sources ? (
    let
      lock = builtins.fromJSON (builtins.readFile ./flake.lock);
      fetch =
        node:
        let
          i = lock.nodes.${node}.locked;
        in
        if i.type == "github" then
          builtins.fetchTarball {
            url = "https://github.com/${i.owner}/${i.repo}/archive/${i.rev}.tar.gz";
            sha256 = i.narHash;
          }
        else if i.type == "git" then
          (builtins.fetchGit { inherit (i) url rev; }).outPath
        else
          throw "shell.nix: unsupported input type '${i.type}' on node '${node}'";
    in
    assert lock.version == 7;
    builtins.mapAttrs (_: fetch) lock.nodes.root.inputs
  ),

  system ? builtins.currentSystem,

  pkgs ? import sources.nixpkgs { inherit system; },
}:

pkgs.mkShell {
  packages = with pkgs; [
    # project dependencies go here
  ];

  shellHook = ''
    # If a uv/virtualenv .venv exists, point tools at it. An editor launched
    # from inside this shell (helix, zed, vim) inherits VIRTUAL_ENV and PATH,
    # so pyright/pylsp resolve imports, hover, signatures, and go-to-source
    # against the project's actual installed packages — no per-editor config,
    # and the language server itself comes from the dev env (`shell -w python`).
    if [ -d "$PWD/.venv" ]; then
      export VIRTUAL_ENV="$PWD/.venv"
      export PATH="$VIRTUAL_ENV/bin:$PATH"
    fi

    # export PROJECT_ROOT="$PWD"
  '';
}
