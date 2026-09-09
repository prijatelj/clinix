# Clinix: CLI for Nix Environment Management

Clinix cures the confusing Nix ecosystem fragmentation by adding yet another standard that brings together the existing standards! :P

Clinix is a configurable command line interface (CLI) written in Rust that manages Nix environments natively and by shelling out to common tools such as non-experimental Nix, `git`, and `curl`.
Clinix pins [nixpkgs](https://github.com/NixOS/nixpkgs) versions with a `flake.lock` while being usable by `shell.nix` without requiring any experimental Nix features.
The environments are `shell.nix` files that can be layered on top of each other or used in isolation.
Flake users can also use the generated `flake.nix` wrapper of the `shell.nix` file, letting an environment be used by either `nix-shell` or `nix develop` commands.

A project environment consists of
- `shell.nix` that specifies the project runtime environment
- `flake.lock` to pin package versions for both `shell.nix` and`flake` users
    - While the default, it is also optional as its info can be saved within the `shell.nix`.
- An optional `flake.nix` wrapper of `shell.nix` to support flake users
- An optional `.dev/` for the development tools for that project with its own shell.nix.

This cross-support is achieved by `shell.nix` parsing the `flake.lock` package versions and using them to build the environment.
The `flake.lock` can be as exact as desired, with the minimal case being at least pinning the NixOS version to pull packages from when used by the `shell.nix`.

When clinix is used to provide both a default shell.nix and flake.nix in this way, all the derivatives of Nix can use the environments from this tool, including [Snix](https://github.com/SamNet-dev/snix), [Lix](https://github.com/lix-project/lix), [Determinate Nix](https://github.com/DeterminateSystems/nix-installer), and [Flox](https://github.com/flox/flox).
Other version Nix package pinning tools, such as [niv](https://github.com/nmattia/niv) and [npins](https://github.com/andir/npins), are only supported if they can import/export their pinned versions from/to a flake.lock.
[Nix profiles][nix-profile] and [home-manager][] are not intended to be supported as they can be replaced by a well managed `shell.nix`, which Clinix can manage for you.

## The CLI

Clinix consists of multiple sub-commands
- `clinix env`: to manage environments in user space.
    - to manage development environments
    - to manage a project's runtime or test environment
    - **Unimplemented**: user's home environment (replaces HomeManager)
- **Unimplemented** `clinix sys`: to manage a NixOS configuration using the 2026-05 introduced feature `system.nix` isntead of nix-channels, letting your configuration be entirely declarative in isolation.

Each subcommand includes the following

- `init` to initialize an environment's configuration
- `add` and `remove` to add or remove packages to an environment
- `pin` and `unpin` to pin or unpin package versions
- `update` to update the unpinned packages to their latest versions.
- `import` and `export` the environment from/to other configuration specifications or container objects, such as Dockerfiles or images.
    - Import or export a nix closure of an environment
    - Import an existing shell.nix to be a wrapped by a flake.
    - generate a shell.nix from a flake.nix and its flake.lock
- `info` for information on an environment's packages or other diagnostic tools
    - shared packages between a set of shells.
    - environment check

### Limitations of the CLI

Clinix does not replace modifying the configration files entirely.
`clinix sys` will not fully support the add/remove package commands because NixOS configuration is typically more complicated and often involves editting the configuration of the system of packages directly.
The CLI will simply help maintain them or get started with broad strokes for common cases.
Intriciate configurations will require modifying the `*.nix` files directly.


### Garbage Collection

A plain `nix-shell` does not register a GC root, so `nix-collect-garbage` deletes a project environment's closure the moment you leave it, which forces a redownload/rebuild on the next entry.
To preserve an environment with clinix, you create a new environment from that project or shell.nix using `clinix env new YourProject --from ./path/to/project/dir/or/shell.nix`.
If you had already entered that project, `new` **adopts its existing build** — it roots the registry env reusing the already-built store paths, so there is **no re-entry and no rebuild** (add `--clean` to also release the source project's own roots; otherwise clinix just notes how). Once rooted, its packages are saved.
`clinix env clean` releases them, and then `nix-collect-garbage` reaps what nothing else keeps.
No global `nix.conf` changes required.

#### The versioned root pairs clinix writes

Entering an env with `clinix env shell`/`run`, or the bare-name sugar, writes GC roots
into the state dir's `roots/`, defaulting to `~/.local/state/clinix/roots/`.
Each root is keyed by the env's identity:
- `env-<name>` for a registry env,
- `proj-<slug>` for a project directory,
- `file-<slug>` for a `*.nix` file target,
- `stack-<names>` for a seed/union composition.

Roots are **versioned**. Each entry that changes the derivation mints the next version
`@<seq>` as a **pair** of indirect roots; the highest `<seq>` is the *current* version:

- `roots/<key>@<seq>`
    - points at that version's `.drv`
    - Pins the **evaluation/source graph** (e.g. the pinned nixpkgs), so the env can be re-evaluated and rebuilt from source.
- `roots/<key>@<seq>.rt`
    - points at the realized output of the shell's [`inputDerivation`](https://github.com/NixOS/nixpkgs/pull/95536)
    - Pins the **complete built closure**: `stdenv`, `bash`, and every package with their transitive dependencies. This is the retention guarantee, and it holds **regardless of the `keep-outputs` setting**.

Both are *indirect* roots where Nix registers a matching entry under
`/nix/var/nix/gcroots/auto/` pointing back at these files, so removing the file in
`roots/` is all it takes to release that version.
The `.rt` root is why clinix does **not** need `keep-outputs = true`: `inputDerivation`'s runtime dependencies *are* the shell's build-time dependencies, so rooting it keeps the whole environment alive by ordinary closure-based GC.
Versions are minted **only when the derivation actually changes** — re-entering an unchanged env reuses the current version and does no work.

#### Keeping prior versions: `keep_n_prior_roots`

By default (`0`), when a new version replaces the current one the old version is
released immediately, so `nix-collect-garbage` can reap its closure. To keep previous
versions around for fast — and **offline** — switch-back, set in `config.toml`:

```toml
[env.gc]
keep_n_prior_roots = 2            # named registry (env-*) + file (file-*) roots; default 0
keep_n_prior_project_roots = 0    # project (proj-*) roots; negative = don't root projects at all
keep_n_prior_stack_roots = 0      # stack/union (stack-*) roots; negative = don't root unions
```

The three settings let each **kind** of root have its own policy. Projects (entered by
path) and composed unions are more ephemeral than named envs, so you can keep fewer of
them, or **disable their rooting entirely with a negative value** (a `-1` project
setting means entering a project behaves like a plain `nix-shell` — GC-collectible).

Because each version keeps both its `.drv` *and* its `.rt`, a prior retains the recipe
**and** the built packages, so it can be re-entered offline straight from its stored
derivation (no evaluation). List and enter them:

```sh
clinix env roots web                 # list one env's versions: current + retained priors, with ids
clinix env roots                     # (no name) list ALL root families grouped by kind
clinix env shell web --prior 1       # enter the version just before current (offset)
clinix env shell web --root-version 4  # enter an exact version by its id
```

> **Accumulation:** each retained version pins a full `stdenv`. Higher
> `keep_n_prior_roots` (and envs you keep changing) leave older closures rooted until
> pruned or `clean`ed, so `/nix/store` grows — by design (nothing you saved is deleted
> behind your back). Keep the limit modest and release what you no longer need.

#### Releasing envs and collecting

```sh
clinix env clean web                     # release ALL of the `web` env's versions
clinix env clean rust python a           # several envs, each resolved independently
clinix env clean web --root-version 4    # only version @4 (exact, from `env roots`)
clinix env clean web --oldest 2          # only the 2 oldest versions, keep the rest
clinix env clean --union a b             # the composed union `stack-a-b` (sorted)
clinix env clean -o a b                  # the ORDERED union `stack-a-b` (as `shell -o a b` made it)
clinix env clean --projects              # bulk: every project (proj-*) root
clinix env clean --stacks                # bulk: every stack/union (stack-*) root
clinix env clean --match '*-rust*'       # bulk: every root whose key matches the glob
nix-collect-garbage                      # now reap every store path no root keeps
nix-store --optimise                     # optional: hardlink-dedup identical files
```

Target selection is one of three modes:
- **Per-name** (`clean a b c`) — releases **each** name's own versions in turn (it does *not* target a `stack-a_b_c` union).
- **Union** (`--union`, or the global `-o`) — treats the names as one composition and releases the single `stack-<…>` root a `shell`/`run` of the same names created (sorted, or ordered under `-o`).
- **Bulk** (`--projects`/`--stacks` by kind, `--match <glob>` by key) — releases many families at once; takes no names.

Within any target, `--root-version <id>` releases one exact version and `--oldest <N>`
releases the N oldest (both require a single target). Releasing touches only the
symlinks; the env's `shell.nix`/`flake.lock` are untouched, and a name/version with no
root is a reported no-op rather than an error. Use `clinix env roots` (no name) to
discover project/file/stack roots, which have no registry listing.

Diagnostics for reasoning about the store:
```sh
nix-store --gc --print-roots         # every root and what it points to
nix-store --gc --print-dead          # dry run: what would be freed
nix-store -q --roots /nix/store/PATH # why is this specific path retained?
du -sh /nix/store
```

## Design

Clinix is meant to provide a CLI that unifies and streamlines the common use of Nix environments using vanilla Nix `shell.nix` while working well with experimental `flake.nix` repositories.
There are many Nix tools and different ways to do the same thing in Nix without a definite upstream standard.
Most of these different approaches added a benefit or convenience not provided before.
Clinix is intended to cut through the excess and get to the core of default Nix use without discarding the usefulness provided by these other standards or tools.

Because there are many ways to do things that work, Clinix is designed to support the most Nix users by supporting both shell.nix and flake.nix while minimizing the actual dependencies and standards required.

### Why not commit to flakes?

While flakes are often used due to their immediate declarative functionality for projects, they have some issues, which are mostly covered in [jade.fyi's blog post](https://jade.fyi/blog/flakes-arent-real/) with practical advice.
- They are still experimental and are not accepted as the standard
    - But this is for various reasons, in part to those that follow
- Implementation Issue: Flakes can cause duplicate packages or environments which wastes space.
- Design Issue: Flakes need to be coupled with a repository's `git`, and cannot be used without a git repository. Nix shells can.
- The typically desired features of flakes can be provided by shell.nix.
    - Main issue is it is not clearly documented how or tools (like Clinix) don't exist to make that easy for the common cases in practice.

Therefore, in searching for a single standard that covers most of the use cases, non-experimental Nix provides the most and can even use `flake.lock` information.
Thus, we can still use the work done by flake.nix users and give them an easy way to use what we create.

### Why Rust?

My friends, colleagues, and I each have a plethora of experience in the pains of working with languages that are not statically typed by default, like Python, Java Script, and the Nix language itself.
Typed systems encourage proper design, catch bugs earlier, and can be more efficient at runtime.
Given this and for my own sanity, I wrote this in Rust.


## Semantic Versioning 2.0.0

[Semantic Versioning 2.0.0](https://semver.org/)
> Given a version number MAJOR.MINOR.PATCH, increment the:
> 1. MAJOR version when you make incompatible API changes
> 2. MINOR version when you add functionality in a backward compatible manner
> 3. PATCH version when you make backward compatible bug fixes.

Please note SemVer 0 semantics,
> Major version zero (0.y.z) is for initial development. Anything MAY change at any time. The public API SHOULD NOT be considered stable.
along with an added note on when we'll increment to a major version:

Major version 0 will not be incremented until the initial intended feature set is
- fully developed,
- documented,
- test covered (unit, integration, end-to-end), and
- tested in practice (daily driving).

Here documentation includes a properly informative --help in the CLI itself along with refined docs in the code and their autogenerated man pages and website for the project.

## Road Map

0. **v0.0.0**: Basic functionality for daily driving environment management and use, project initialization, and export of environments to Nix closure and Docker images.
    - Generalized the shell script prototype into the initial Rust project structure.
    - No system tools, no user home helper tools yet.
1. **Package Dependency Resolution**
    - We want to be able to give `clinix` a versioned package manifest (e.g., TOML) and it finds the best nixpkg version to use for the flake.lock.
        - Today, this is best achieved by [Flox](git@github.com:flox/flox.git).
    - We also want that package dependency resolution to be informed by vulnerabilities (CVEs) and weaknesses (CWEs), such that the user is informed about the risks of using certain package version configurations, and are guided to the latest informaiton on how to resolve those issues.
    - Automate a regular fetching of package information such that the user can be informed when updates exist or vulnerabilites are found in the packages they use.
    - Similarly, offer this for general git repository tracking
        - fetch revisions, references, tags, and maybe releases
    - Nix package searching, possibly shelling out to [nh][]
        - Search should provide ease of finding packages or their nixpkg names (fuzzy search) as well as browsing by tags and traversing the dependency graph.
        - Ideally, this supports both offline and online setups, favoring local cache when up-to-date and available.
2. **User home environment management**
    - This is technically feasible today, but `clinix` should provide some tools to help manage this, which includes setting which environment(s) load at login and managing dot/configuration files for the packages used (possibly a separate crate).
        - Some of this may just be documentation, e.g., "run `clinix env shell your-home-env` in shell rc, or xinit, or whatever is Desktop Environment's equivalent for initial scripts at login."
    - Why not [home-manager](https://github.com/nix-community/home-manager)?
        - Because what home-manager provides is achievable with a just `shell.nix` launched at login.
        - It introduces another standard that drifts from both NixOS and nixpkgs.
        - We want to focus on as few extra standards as possible while maintaining the beneficial features they provide.
    - "Profile roll-back": It may be useful to implement the saving of drvs similar to profiles or like for NixOS, but the environments only get deleted when the user chooses garbage collect and the environments are meant to be stored in version control systems (point here is to avoid re-building). So a better answer may be more nuanced and more easily controlled / configured garbage collection CLI.
        - We do not and do not intend to support [nix profiles][nix-profile].
3. **System environment helper tools**
    - Clinix prioritizes `system.nix` use for fully declarative NixOS configurations, and thus do not use nix-channels.
    - A clean separation of concerns.
    - Help the user separate their system from their user environments.
        - So their system doesn't have unnecessary system-wide packages or dependencies
        - So their user environments are portable for NixOS nix use.
    - Export closures and to Docker images using NixOS.


[home-manager]: https://github.com/nix-community/home-manager
[nix-profile]: https://nix.dev/manual/nix/2.34/command-ref/new-cli/nix3-profile.html
[nh]: https://github.com/nix-community/nh
