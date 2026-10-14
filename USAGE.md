# clinix — Usage

`clinix` manages Nix environments — projects, reusable named envs, and personal
shell fragments — and the pin that makes them reproducible. It is a single Rust
binary that shells out to **classic, non-experimental Nix** (`nix-shell`,
`nix-instantiate`, `nix-store`, `nix-prefetch-url`, `git`).

The central claim: a `flake.lock` is just JSON, so **classic Nix can read it and
clinix can write it**. You get flake-grade reproducibility — one pinned nixpkgs
rev fixing every package version in the closure — while `nix-shell` stays the
entry point and flakes stay optional. `nix flake update` and `clinix env update`
are interchangeable writers of the same file; neither keeps private state.

This document is task-oriented: what to run to get something done, and where you
must instead hand-edit a `.nix` file or reach for vanilla `nix-shell`. It
supersedes `~/dev_env/USAGE.md`, which documented the shell-script prototypes
(`pin`, `shell`, `deps`, `envcheck`, `shareck`, `export-docker`) that clinix has
reimplemented and extended.

---

## 1. The CLI layout

```
clinix [-v] [-o] [-r] [--config DIR] [--state-dir DIR] <command | names…>

  env <verb> …            manage Nix environments            (the main scope)
  sys <verb> …            manage a NixOS system.nix config
  info [check|deps]       report the shell you are standing in
  config <path|state|example>
  completions <shell>     emit a completion script
  man [dir]               write clinix.1 (roff)

  <names…>                sugar → `env shell <names…>`
```

`clinix env` groups its verbs three ways (the grouping `env --help` prints):

| group | verbs |
|---|---|
| **management** | `init` `new` `rename` `add` `remove` `flake` `update` `outdated` `verify` `import` `export` `clean` |
| **execution** | `shell` `run` |
| **diagnostics** | `list` `info` `deps` `shared` `check` `roots` |

Global flags are declared once at the top level and read by whichever verb needs
them, so the bare-name sugar can still carry them:

| flag | meaning |
|---|---|
| `-o`, `--ordered` | compose in the order given instead of sorting lexically (later entries win on `PATH`) |
| `-r`, `--runtime` | *accepted but currently inert* — see the note on project composition in §6 |
| `-v`, `--verbose` | repeatable |
| `--config DIR` | override the config dir (else `$CLINIX_CONFIG_DIR`, `$XDG_CONFIG_HOME/clinix`, `~/.config/clinix`) |
| `--state-dir DIR` | override the state dir (else `$CLINIX_STATE_DIR`, `$XDG_STATE_HOME/clinix`, `~/.local/state/clinix`) |

Two things worth knowing immediately:

- **`clinix` with no arguments prints help** — it does *not* enter the current
  directory. Enter the cwd project explicitly with `clinix .` or `clinix env shell`.
- **The bare-name form is terminal.** `clinix rust claude` composes those envs;
  no verb may follow the names. A name that collides with a top-level command
  (`env`, `sys`, `info`, `config`, …) is taken as that command — use
  `clinix env <name>` for such an env.

### Configuration

```sh
clinix config path                      # which config.toml clinix reads
clinix config state                     # where envs, GC roots, compose files, the seed pin live
clinix config example --full > ~/.config/clinix/config.toml
```

`config.toml` mirrors the command tree (`[env]`, `[env.seeds]`, `[env.gc]`). A
root-level `use = ["other.toml"]` merge-imports other config files: imports are
the base, the importing file overrides them, later `use` entries win over
earlier, list fields concatenate with the local file first, and cycles error.

---

## 2. The model: three kinds of environment

Everything `clinix env <name>` can enter is one of three node kinds. Knowing
which you are holding explains every behavioural difference that follows.

```
                    ┌──────────────────────────────────────────────┐
   project          │ a DIRECTORY you own                          │
   (Kind::Project)  │   shell.nix  [+ flake.lock] [+ flake.nix]    │
                    │   found by path or cwd; no registry name     │
                    │   self-contained: reads its OWN lock         │
                    └──────────────────────────────────────────────┘
                    ┌──────────────────────────────────────────────┐
   seed             │ a *.nix FRAGMENT in your seed sources        │
                    │   `{ pkgs }: pkgs.mkShell { … }`             │
                    │   read in place; named by basename           │
                    │   NOT self-contained: clinix supplies pkgs   │
                    └──────────────────────────────────────────────┘
                    ┌──────────────────────────────────────────────┐
   registry env     │ state/envs/<name>/ — a NAMED, reusable env   │
   (Kind::Registry) │   shell.nix + flake.lock (its own pin)       │
                    │   may hold `namespace:member` subshells      │
                    │   self-contained: reads its OWN lock         │
                    └──────────────────────────────────────────────┘
```

**Target resolution** for each token, in precedence order:

1. an explicit `*.nix` **file** — checked first, so `./dev.nix` runs the file;
2. a **directory-backed env** — `.`/empty = the cwd project, a `/`-bearing or
   existing path = a project there, `namespace:member` = a registry subshell, a
   bare name = `state/envs/<name>` if it exists;
3. a **seed** from the catalog — `namespace:name`, or a bare name.

A registry env therefore **shadows a same-named seed**. `clinix env new` and
`clinix env init` warn when the name you are creating would introduce that shadow.

**The pin is the version list.** A nixpkgs rev is a fixed point: it pins every
package's version and build recipe at once, so there is no per-package version
file to keep. Package versions are *derived* — `clinix env info` evaluates them
on demand — not recorded.

---

## 3. Creating a project

A project is a directory. `init` scaffolds one and never overwrites an existing
`shell.nix`, `flake.lock`, or `flake.nix` — it clobber-checks every target
*before* the network resolution, so a failure leaves the directory untouched.

```sh
cd ~/projects/myproj
clinix env init                                  # shell.nix + flake.lock, nixpkgs tracking nixos-26.05
clinix env init --nixpkgs nixos-unstable         # track a different branch
clinix env init --nixpkgs 062346a6d8…            # a 40-hex rev → frozen from birth
clinix env init -p ripgrep -p nodejs             # seed the packages list
clinix env init ~/projects/other -q              # elsewhere, without progress output
```

### The three file layouts

| layout | files | who can enter it |
|---|---|---|
| **default pair** | `shell.nix` + `flake.lock` | `nix-shell`, `clinix` |
| `--flake` | `shell.nix` + `flake.lock` + `flake.nix` | the above, plus `nix develop` |
| `--shell-only` | `shell.nix` (lock embedded) | `nix-shell`, `clinix` |

**How the three files relate.** `shell.nix` is the single structural source of
truth; `flake.lock` is its pin; `flake.nix` is an optional front door. The
generated `shell.nix` takes three arguments, all defaulted, which is what lets
one file serve every caller:

```nix
{
  sources ? ( /* inline flake.lock reader */ ),
  system  ? builtins.currentSystem,
  pkgs    ? import sources.nixpkgs { inherit system; },
}:
pkgs.mkShell { packages = with pkgs; [ … ]; shellHook = ''…''; }
```

- Under bare `nix-shell`, no caller supplies anything, so the defaults fire and
  the inline reader parses `./flake.lock` itself. The Nix evaluator has no lock
  reader, so that translation has to be written in the language — it converts
  each node to a `fetchTarball` (verifying `locked.narHash`, which is exactly the
  NAR hash of the unpacked tree) or a `fetchGit` by rev.
- Under `nix develop`, Nix has already fetched the inputs and passes `sources`
  in, so the reader is never evaluated. **One lock, one nixpkgs instance, no
  second source of truth.**
- Under `clinix`, the launcher supplies what it resolved and GC-roots the result.

`flake.nix` adds *no* pin of its own: `clinix env init --flake` writes a wrapper
whose `devShells.default` is `import ./shell.nix { … }`, reading the same lock.
Both `nix flake update` and `clinix env update` write that lock, and they
converge because neither holds extra state.

**`--shell-only`** embeds the lock as a `builtins.fromJSON ''…''` here-string
instead of a sibling file — one self-contained, copy-pasteable file for vanilla
`nix-shell`. The trade: the lock is no longer a file other tooling can read, so
the mutating lock verbs refuse it (see §7). Read-only verbs still work, and
`clinix env flake <env> wrap-shell` can later **expand** a shell-only env back
into the three-file form.

### Already have a `shell.nix`?

Then you do not need `init`. Three onward paths:

```sh
clinix env flake . wrap-shell            # add a flake.nix front door (auto-detects the arg style)
clinix env new myproj --from .           # register it under a name (§5)
clinix .                                 # just enter it, GC-rooted
```

`wrap-shell` classifies the shell by its formal arguments
(`builtins.functionArgs`), overridable with `--arg-style`:

| class | the shell takes | what the flake does | lock |
|---|---|---|---|
| `clinix-native` | `sources`/`system`/`pkgs` | passes all three | reuses the existing lock, never clobbers |
| `conventional` | a `pkgs` argument | `pkgs = import nixpkgs { inherit system; }` | pins nixpkgs fresh (`--nixpkgs`, default `nixos-26.05`) |
| `impure` | nothing injectable | `import ./shell.nix { }` | none — needs `nix develop --impure`, requires `--impure` to opt in |

The `impure` class is refused without the explicit flag rather than silently
producing an unreproducible wrapper.

---

## 4. Seeds — your personal shell fragments

A **seed** is a `*.nix` file you keep, discovered from config and read **in
place**. Seeds are how you carry editor/language/tool shells across every project
without baking them into any of them.

### Declaring where they live

```toml
[env]
nixpkgs = "nixos-26.05"                              # a git REF clinix locks (never a channel)
# nixpkgs = { flake_lock = "~/nixos/flake.lock" }    # or: an existing lock is authoritative
# nixpkgs_config = "~/.config/nixpkgs/config.nix"    # allowUnfree/CUDA for seed builds

[env.seeds]
sources = [
  "~/.config/clinix/seeds/",                          # a dir: each *.nix is a seed
  { path = "~/dots/clinix/seeds", namespace = "dots" },# table form sets a namespace
  "~/one-off/my-python.nix",                          # an exact file: one seed
]
ignore = ["_*.nix", "lib/**"]                          # also honoured via a .clinix_ignore per dir
```

A directory source is scanned **recursively**; each `*.nix` becomes a seed named
by **basename** (`seeds/lang/rust.nix` → `rust`). Subdirectories organise but do
not qualify names, so two files sharing a basename collide: clinix warns,
resolves the bare name by source order (first wins), and names the selector for
the other.

The `ignore` globs matter more than they look: a helper `.nix` that is *not* a
shell would otherwise be catalogued as a bogus seed. A pattern containing `/`
matches the path relative to the source; without `/` it matches the basename at
any depth.

### Writing one

A seed is a fragment that takes what clinix provides:

```nix
# ~/.config/clinix/seeds/rust.nix
{ pkgs }:
pkgs.mkShell {
  packages = with pkgs; [ rustc cargo rust-analyzer clippy ];
}
```

Creating a seed is just **writing the file** — there is no `clinix` verb for it,
and nothing to register. It appears in `clinix env list` immediately.

### Using them

```sh
clinix env list                  # registry envs + seeds (NAME / NAMESPACE / SOURCE)
clinix rust                      # enter one seed
clinix rust claude               # union of both, one shell (sorted lexically)
clinix -o rust claude            # preserve the given order
clinix dots:claude               # the namespace-qualified selector
clinix ~/one-off/my-python.nix   # the exact-path selector (always works)
```

Seeds compose by **union**: clinix writes a compose expression to
`state/compose/stack-<labels>.nix` that imports the pinned nixpkgs once and joins
each fragment via `mkShell`'s `inputsFrom`. **One pin per composed stack** — so no
two packages in a stack can disagree about a shared library's ABI.

### The seed pin

Seeds have no lock of their own; clinix supplies one:

- `[env].nixpkgs = "<ref>"` — clinix resolves and locks it into
  `<config>/flake.lock` on first use (network, once). Committable; it is your pin.
- `[env].nixpkgs = { flake_lock = "<path>" }` — that file is authoritative and
  clinix never writes it. This is how one lock governs your system *and* your
  seeds.

Unfree packages (e.g. `claude-code`) need a nixpkgs `config`. Set
`[env].nixpkgs_config` to a file evaluating to a config attrset; when unset,
clinix passes no explicit config, so nixpkgs falls back to its own default
(`~/.config/nixpkgs/config.nix`).

---

## 5. Registry envs — named, reusable, and promoting a project

A registry env lives at `state/envs/<name>` and is entered by name from
anywhere. `clinix env new` creates one, in **two modes** chosen by what `--from`
names.

### Mode A — compose seeds into a frozen env

```sh
clinix env new work --from rust claude python
```

`--from` naming seeds materialises them: each fragment is **copied** into
`state/envs/work/seeds/`, the seed pin is copied to `state/envs/work/flake.lock`
(plus `nixpkgs-config.nix` if configured), and a self-contained `shell.nix` unions
the copies. The result is **portable and deliberately frozen** — it imports its
own copies, not your sources, so editing `~/.config/clinix/seeds/rust.nix` later
does not change it. Refresh by re-running `new --from`.

Contrast with `clinix rust claude python`, which reads the fragments **live** —
Nix re-evaluates on every entry, so a seed edit takes effect immediately.

### Mode B — register an existing shell (promoting a project)

```sh
clinix env new myproj --from .                   # reference the cwd project by absolute path
clinix env new myproj --from . --copy            # freeze a snapshot into the registry instead
clinix env new myproj --from ~/projects/myproj --clean
clinix env new myproj:dev --from ./dev.nix       # a member subshell sharing myproj's pin
```

`--from` naming a single directory or `*.nix` file registers it. By default the
registry env is a **thin wrapper importing the source's absolute path**, so the
project stays the source of truth and edits flow through; `--copy` freezes a
snapshot.

**Build adoption** is the part that makes promotion cheap: if you had already
entered the source and its current build matches the new env's derivation, clinix
roots the registry env from that existing closure — *no rebuild, no re-entry*. It
then either releases the source's own GC roots (`--clean`) or tells you how.

**Lock coherence.** A namespace shares one pin (`state/envs/<ns>/flake.lock`),
and members read it as `../flake.lock`. clinix parses the source `shell.nix` for
a `readFile …/flake.lock` and decides:

| the source's lock | result |
|---|---|
| none, or points at a missing file | warn — registered **unpinned**, entered as-is |
| present, namespace has no pin yet | **establish** the namespace pin from it |
| identical to the namespace pin | **reuse** (dedup) |
| differs from the namespace pin | **error** — pass `--overwrite` (repin the namespace) or `--use-prior` (this shell conforms) |

A `namespace:member` target may create its namespace empty (with a warning), so a
dev shell can be staged before the runtime shell is registered.

### Managing registry envs

```sh
clinix env list
clinix env rename work workstation     # mv + O(1) GC-root re-key
clinix env info work                   # pin + resolved package versions
clinix work                            # enter it
```

`rename` is correct by construction because registry envs key their GC root by
**name** (`env-<name>`), while projects key by **path slug** (`proj-<slug>`) — a
path-slug key would orphan the root on rename.

---

## 6. Entering and running

```sh
clinix .                               # the cwd project, GC-rooted
clinix env shell                       # the same
clinix work                            # a registry env
clinix rust claude                     # a seed union
clinix env shell work --pure           # nix-shell --pure
clinix env run work -- cargo test      # non-interactive; mirrors the exit code
clinix env shell work --prior 1        # the version before current (offline-capable)
clinix env shell work --root-version 4 # an exact version by id
```

An interactive `shell` **replaces** the clinix process with `nix-shell`, so no
clinix node lingers between your shell and the nix-shell — process-tree tools see
the real layering. `run` instead forks and waits so it can mirror the command's
exit code, making it safe in scripts and CI.

### Ad-hoc packages: `-p`, the `nix-shell -p` shape

Add packages to whatever you are entering, without editing any file:

```sh
clinix -p tokei                        # just tokei, pinned by the active lock
clinix claude -p tokei                 # the claude env + tokei
clinix env shell claude -p tokei       # identical — the sugar reroutes here
clinix env run rust -p tokei -- tokei .
clinix claude --pkg tokei rust --pkg fastfetch python   # interleaved
```

Two spellings, because the name list beside them is also variadic:

| form | behaviour |
|---|---|
| `-p` / `--pkgs` | **greedy** — every remaining bare token is a package, not an env |
| `--pkg <one>` | exactly one package; bare tokens after it are envs again |

**Which pin resolves them.** Whatever lock is active at the layer you are on: the
env's own lock for a project/registry/file target, the shared lock of a mixed
union, and — with no envs named at all — the cwd project's lock if there is one,
else the seed pin. So `clinix -p tokei` works in any directory.

**They are ephemeral.** A `-p` shell is entered **unrooted**, exactly as plain
`nix-shell -p` is, so `nix-collect-garbage` reclaims it. Nothing is written to
your envs. To keep one, register it:

```sh
clinix env new tools --from claude rust -p tokei fastfetch   # seeds + packages
clinix env new tools -p tokei fastfetch                      # packages only
```

That bakes the packages into the new env's committed `shell.nix`, where
`env add`/`env remove` take over from then on.

**Versions are rejected.** `clinix -p tokei=1.2` errors. `nix-shell -p` has no
such syntax either — its arguments are "interpreted as attribute names inside the
Nix Packages collection" — so pick the versioned *attribute* (`nodejs_20`), or pin
declaratively, which is what `shell.nix` + `flake.lock` and `env add` are for.
Attribute paths work: `-p python313Packages.black`.

### Entering a project composes *only* that project

This is the sharpest behavioural difference from the `~/dev_env` prototype, whose
`shell` unioned the dev tools with the project runtime automatically.

`clinix .` enters the project's own `shell.nix` and nothing else. To get your
editor and language servers alongside the project's interpreter, **name the seeds
explicitly**:

```sh
clinix .                               # the project alone (prototype's `shell -r`)
clinix python claude .                  # project ∪ those seeds (prototype's `shell -w python -w claude`)
clinix -o python claude .                # same, in the given order
```

The implicit project-mode composition (`compose.nix` — project runtime ∪ dev
shells) is deferred, which is why `-r/--runtime` is parsed but not yet acted on:
with no implicit union to opt out of, there is nothing for it to do. Naming the
seeds is the supported form and is strictly more explicit.

### Mixing kinds in one union

Seeds take the shared `pkgs`; a project, registry env, or `*.nix` file reads its
own `flake.lock`. Two consequences:

**The self-contained member's lock governs the whole union.** In
`clinix python claude .`, the seeds are built against **the project's**
`flake.lock`, not the seed pin — so your tools are built against exactly the
nixpkgs the project pinned. (With no self-contained member, an all-seed union
falls back to the seed pin from §4.)

**Every self-contained member must share one pin.** Differing locks are a hard
error, not a silent multi-pin environment:

```
clinix: not yet implemented: composing envs with differing flake.locks
  only a shared flake.lock is supported at this time — every env in a union must share one pin
```

A corollary: a **`--shell-only` project cannot join a union at all** — the
shared-pin check needs a `flake.lock` file to compare, and a shell-only env's
lock is embedded in its `shell.nix`. Such an env enters fine on its own
(`clinix .`); to compose it, expand it first with
`clinix env flake . wrap-shell`.

**What clinix adds over plain `nix-shell`.** Vanilla `nix-shell` works fine on
every file clinix writes, and you should reach for it freely. What you lose is
exactly: the GC rooting (§8), the union composition, name resolution, and the
registry. For a genuine one-off, `nix-shell -p jq` remains the right tool —
there is no reason to involve clinix.

---

## 7. Changing an environment: packages and pins

Two independent axes. **Packages** are edits to `shell.nix`; **pins** are edits
to `flake.lock`.

### Packages (edits `shell.nix`)

```sh
clinix env add work ripgrep jq         # append to the packages list
clinix env remove work jq
clinix env add work ripgrep --sort     # also reformat the list lexically
```

These are **offline, span-preserving splices**: clinix parses with `rnix` only to
*locate* the `packages = with pkgs; [ … ]` list and its element spans, then edits
the original text. Comments, formatting, your hand edits, and a shell-only env's
embedded lock all stay byte-identical. The write is atomic (temp + rename), and
the operations are idempotent — already-present or already-absent names are
reported as skipped, not errors.

**Hand-edit `shell.nix` for everything else.** `add`/`remove` understand exactly
one construct: the `packages` list. Anything else is a normal file you own:

- `shellHook`, environment variables, `VIRTUAL_ENV` wiring
- `buildInputs` / `nativeBuildInputs` distinctions, `inputsFrom`
- overlays, `overrideAttrs`, a package from a second nixpkgs input
- any non-`mkShell` shape

Versioned specs (`clinix env add work nodejs=20.11`) are **rejected**, not
silently dropped — per-package version pinning is a later phase.

### Pins (edits `flake.lock`)

```sh
clinix env outdated work               # read-only: which tracked inputs have newer upstream revs
clinix env update work                 # advance every tracked input to its branch's latest rev
clinix env update work --dry-run       # preview (resolves revs; skips the hash prefetch and the write)
clinix env verify work                 # re-hash every locked input against its recorded narHash
```

`update` walks the lock's **root github inputs that track a branch/tag**, resolves
each to its newest commit, and rewrites that node — each input along *its own*
`original.ref`, so an input tracking `nixos-26.05` stays on 26.05. Frozen
(rev-pinned) and non-github inputs are left alone. The lock is written only if
something actually advanced.

The re-lock engine lives once in `relock.rs` and is shared by `env update` and
`sys update`, so both scopes advance a lock identically.

Two present limits worth planning around:

- **No per-input selector.** `update` advances *every* tracked input. To
  re-resolve just one, `flake <env> rm <input>` then `flake <env> add …` again
  (`add` errors on an existing node name rather than overwriting).
- **`env update <pkg>`** (per-package version update) is not implemented, and
  `update` **refuses a shell-only env** — its lock is embedded in `shell.nix`,
  and writing a `flake.lock` there would create a second, drifting source of
  truth.

### Lock inputs

```sh
clinix env flake work add github NixOS/nixpkgs -b nixos-unstable --name nixpkgs-edge
clinix env flake work add github owner repo --at v1.10.0        # frozen at a tag/rev
clinix env flake work add git https://example.com/x.git -b main --name mylib
clinix env flake work rm mylib
```

`-b` resolves a branch/tag and records a **tracked** input (`update` advances
it); `--at` records a **frozen** one. github uses the tarball fast path
(`nix-prefetch-url --unpack`); `git` uses `nix-prefetch-git` and covers any forge
(Codeberg, GitLab, self-hosted) by rev, which is already a content address.

Because `flake … add` takes any **path** as its target, it also works on a bare
lock directory that has no `shell.nix` — e.g. a `system.nix` config:

```sh
clinix env flake ~/nixos add github NixOS/nixpkgs -b nixos-unstable --name nixpkgs-edge
```

### Freeze / unfreeze

```sh
clinix env flake work freeze                    # pin every tracked input at its current rev
clinix env flake work unfreeze --branch nixos-26.05
clinix env update work                          # advance it again after unfreezing
```

Freezing is a standard lock edit: `original` drops its `ref` and gains the
`locked.rev`, so nothing has anything to advance. Both are **whole-env** (there
is no per-input freeze), and `unfreeze` requires `--branch` because clinix does
not yet remember the pre-freeze ref.

For a `--flake` env the two files must agree, since Nix re-derives inputs from
`flake.nix`. clinix swaps `github:owner/repo/<from>` → `.../<to>` in `flake.nix`
— but **only when that URL appears exactly once**. If it does not, clinix
**reverts the lock edit** and tells you which input to fix by hand, rather than
leaving the pair inconsistent.

> These verbs are the successors to the prototype's `pin` commands:
> `pin update` → `clinix env update` / `clinix sys update`; `pin show` →
> `clinix sys info` / `clinix env info`; `pin add|remove` →
> `clinix env flake … add|rm`; `pin freeze|unfreeze` →
> `clinix env flake … freeze|unfreeze`; `pin init` → `clinix env init`.
> `pin` itself no longer exists.

---

## 8. Garbage collection, disk, and diagnosis

### Why rooting exists

Plain `nix-shell` registers **no GC root**, so `nix-collect-garbage` deletes the
environment's closure the moment you leave — forcing a redownload or rebuild on
the next entry. Every env entered through `clinix` gets rooted instead.

### Versioned root pairs

Each entry that **changes the derivation** mints the next version under
`state/roots/`, as a *pair*:

| root | points at | pins |
|---|---|---|
| `roots/<key>@<seq>` | that version's `.drv` | the evaluation/source graph — re-buildable from source |
| `roots/<key>@<seq>.rt` | its `inputDerivation` output | the **complete built closure** (stdenv, bash, every package + transitive deps) |

The `.rt` root is why clinix needs no `keep-outputs = true`: an
`inputDerivation`'s runtime dependencies *are* the shell's build-time
dependencies, so rooting it retains the whole environment by ordinary
closure-based GC. Because both are kept per version, a prior version is
re-enterable **offline** from its stored recipe (`--prior N`).

Re-entering an unchanged env **reuses** the current version and does no work.
Keys are `env-<name>`, `proj-<slug>`, `file-<slug>`, `stack-<labels>`.

### Retention policy

```toml
[env.gc]
keep_n_prior_roots = 2            # named registry (env-*) + file (file-*) roots; default 0
keep_n_prior_project_roots = 0    # project (proj-*) roots
keep_n_prior_stack_roots = -1     # negative = don't root at all (plain nix-shell behaviour)
```

Each *kind* gets its own policy because projects and ad-hoc unions are more
ephemeral than named envs. `0` releases the outgoing version as soon as a new one
replaces it. A **negative** value disables rooting for that kind entirely — a
`-1` project setting makes entering a project behave exactly like plain
`nix-shell`.

Each retained version pins a full `stdenv`, so a high limit grows `/nix/store` by
design — nothing you asked to keep is deleted behind your back.

### Inspecting what holds disk

```sh
clinix env roots                       # every root family, grouped by kind
clinix env roots --size                # + retained closure size per family (shells to nix)
clinix env roots work                  # one env's versions: current + priors, with ids
clinix env roots work --json
clinix env deps work --size            # derivation, package paths, closure count + bytes
clinix env shared work myproj other    # N-way: shared core vs unique sets, and their disk cost
clinix env info work                   # pin + every package's resolved version
```

`clinix env roots` with no name is the **only** way to discover project, file,
and stack roots — they have no registry entry. `shared` is the dedup lens: shared
paths are content-addressed and cost disk once, so a *low* shared percentage
between similar projects usually means divergent nixpkgs pins; aligning the locks
collapses the unique sets.

### Releasing and collecting

```sh
clinix env clean work                        # release ALL of work's versions
clinix env clean work myproj                 # each name released independently
clinix env clean work --root-version 4       # one exact version
clinix env clean work --oldest 2             # the 2 oldest, keep the rest
clinix env clean --union rust claude         # the composed `stack-claude-rust` root (sorted)
clinix env clean -o rust claude              # the ORDERED union, as `-o` composed it
clinix env clean --projects                  # bulk: every proj-* root
clinix env clean --stacks                    # bulk: every stack-* root
clinix env clean --match '*-rust*'           # bulk: by key glob
clinix env clean work --dry-run              # preview; deletes nothing
nix-collect-garbage                          # now reap whatever no root keeps
nix-store --optimise                         # optional: hardlink-dedup identical files
```

Target selection is one of three modes — **per-name**, **union**
(`--union`/`-o`), or **bulk** (`--projects`/`--stacks`/`--match`) — and they do
not combine. `--root-version`/`--oldest` narrow a single target.

`clean` touches **only the root symlinks**. An env's `shell.nix` and `flake.lock`
are untouched, and a target with no root is a reported no-op rather than an error.

Every `clean` also sweeps `state/compose/`: a generated compose expression whose
key no longer has a GC root is deleted, since the next entry of that composition
rewrites it. That covers both unions whose roots you just released and ephemeral
`-p` shells, which never rooted one at all.

### Auditing the shell you are in

```sh
clinix info                            # describe the active nix-shell (works for ANY nix-shell)
clinix info check                      # contextual PATH/lock audit
clinix info deps                       # contextual closure report
clinix env check work                  # the same audit against a named env
```

Bare `clinix info` reads the live shell from the variables Nix exports (`out`,
`system`, `buildInputs`/`nativeBuildInputs`) — no nix call, no `flake.lock`
needed, and it works whether or not clinix launched the shell.

`check` answers "is the tool I'm running the one I pinned, or did it leak in from
the host?" — the failure mode on non-NixOS where `/usr/bin` sits on `PATH`. It
flags only host directories appearing **before** a store directory, since only
those can shadow a pinned tool.

### Vanilla diagnostics

```sh
nix-store --gc --print-roots           # every root and what it points to
nix-store --gc --print-dead            # dry run: what would be freed
nix-store -q --roots /nix/store/PATH   # why is this path retained?
du -sh /nix/store
```

---

## 9. Interop: moving an environment elsewhere

```sh
clinix env export closure work                       # → ./work.closure (complete env runtime closure)
clinix env export closure work rust --out w.closure  # a union's closure
clinix env export closure work --packages            # just the package delta (smaller)
clinix env import work ./work.closure --shell-nix ./shell.nix --flake-lock ./flake.lock
```

`export closure` serialises an **already-built** env; an unbuilt one hard-errors
and points at building first (`nix-store --export` can only serialize valid
paths). The default seeds from the shell derivation's realized inputs so the
archive can *enter* the shell on the target; `--packages` ships only the package
outputs, assuming the target already provides the base by hash.

`import` registers the env — the clinix-specific, guaranteed part — and populates
the store on a best-effort basis. If nix refuses the store import (multi-user
store, untrusted user), the env is **still registered** and clinix prints the
`sudo nix-store --import` to finish with.

```sh
clinix env export docker work --out ./containers
clinix env export docker work --load                 # build and `docker load`
clinix env export docker work --from nvidia/cuda:12.6.0-base-ubuntu24.04
```

`--from` layers an external base image and emits a **digest-pinned** Dockerfile,
never an unpinned tag.

---

## 10. The `sys` scope

```sh
clinix sys info [dir]                  # the pinned inputs: NAME / TYPE / REV / TRACKING
clinix sys update [dir] [--dry-run]    # advance every tracked input along its own branch
```

A `system.nix` config directory is just a directory holding the `flake.lock` its
inlined reader consumes, plus the NixOS modules — so advancing the system pin
needs neither a `shell.nix` nor the experimental `nix flake` CLI. Both verbs run
the same `relock.rs` engine `env` uses.

`clinix sys init`, `switch`, and `rollback` are **not implemented** — they drive
`nixos-rebuild`; use it directly:

```sh
nixos-rebuild build  --file /etc/nixos/system.nix
nixos-rebuild switch --file /etc/nixos/system.nix
```

---

## 11. Not yet implemented

Every one of these **errors clearly** rather than degrading silently:

| what | status |
|---|---|
| `env init --from <spec>` (adopt a `pyproject.toml`/`Cargo.toml`) | later phase |
| `env init -p pkg=version`, `env add/remove pkg=version` | needs package-level version pinning |
| `env update <pkg>` (per-package version update) | later phase |
| `env update` on a shell-only (embedded-lock) env | re-embedding is a later slice |
| composing envs with **differing** `flake.lock`s | only a shared pin is supported |
| implicit project composition (runtime ∪ dev seeds) and therefore `-r`/`--runtime` | name the seeds explicitly: `clinix python .` |
| `env new <ns>:<member> --from <seeds…>` | register a single shell into a member instead |
| `sys init` / `sys switch` / `sys rollback` | use `nixos-rebuild --file` |
| self-contained **seeds** (a seed bringing its own pin) and flake seeds | deferred together |
| per-input `update`, per-input `freeze` | whole-env today; `flake rm` + `add` is the workaround |

---

## 12. Where things live

| path | what |
|---|---|
| `~/.config/clinix/config.toml` | the config (`clinix config path`) |
| `~/.config/clinix/flake.lock` | the seed nixpkgs pin — generated, committable, unless `[env].nixpkgs.flake_lock` points elsewhere |
| `~/.local/state/clinix/envs/<name>/` | registry envs (`shell.nix`, `flake.lock`, `seeds/`, members) |
| `~/.local/state/clinix/roots/` | GC roots: `<key>@<seq>` + `<key>@<seq>.rt` |
| `~/.local/state/clinix/compose/` | generated seed-stack compose expressions |
| your seed sources | seeds, read in place (`[env.seeds].sources`) |
| a project directory | `shell.nix` [+ `flake.lock`] [+ `flake.nix`] — yours to edit |

`clinix config state` prints all of these resolved.

---

## 13. A typical workflow

```sh
# 1. scaffold and pin a project
cd ~/projects/tfp_jax
clinix env init --nixpkgs nixos-26.05
clinix env add . uv                       # or hand-edit shell.nix for anything structural

# 2. work: the project runtime unioned with your tool seeds, GC-rooted
clinix -o python claude .                 # ordered: later entries win on PATH

# 3. inspect
clinix info check                         # are my tools pinned, or leaking from the host?
clinix env deps . --size                  # what does this closure cost?
clinix env shared . ~/projects/other      # dedup opportunity between projects?

# 4. promote it to a name once it stabilises
clinix env new tfp --from . --clean       # adopts the existing build — no rebuild

# 5. keep the pin current
clinix env outdated tfp                   # read-only check
clinix env update tfp                     # advance; re-entry mints a new root version
clinix env roots tfp                      # current + retained priors
clinix env shell tfp --prior 1            # the previous version, offline

# 6. ship it
clinix env export docker tfp --load

# 7. reclaim disk
clinix env roots --size                    # what is holding what
clinix env clean --projects                # release every project root
nix-collect-garbage
```
