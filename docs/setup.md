# Setting up the repository

How this repository was created, how to clone it, and the Nix and Rust structure it uses. What the project is and why
these choices were made: [`project-overview.md`](./project-overview.md).

**Current state.** Steps 1 and 2 have been done in this repository. Step 3 is the **target structure, and has not been
added yet**. Its files were built and run in a scratch flake on 2026-11-05 on `x86_64-linux`, with these results:

- `nix build .#default` built, and the resulting `worktree` binary ran.
- `nix flake check` passed, including the Clippy and formatting checks.
- `nix develop --command just check` exited 0.
- The Home Manager module evaluated in a test home.

The `aarch64-linux` and `aarch64-darwin` outputs evaluated, but were not built.

## 1. Create the repository

Needs `gh`, authenticated, and `bd` 1.0 or newer.

```bash
gh repo create tomasz-feliksik/worktree-keeper --private \
  --description "Keeps agent git worktrees safe to create, inspect and remove"
git clone git@github.com:tomasz-feliksik/worktree-keeper.git ~/code/projects/worktree-keeper
cd ~/code/projects/worktree-keeper
git symbolic-ref HEAD refs/heads/main
```

Do not run the tracker setup below from a linked worktree: `bd init` there walks up to `~/.beads` and builds the tracker
in the wrong place.

## 2. Set up the tracker

```bash
bd init --non-interactive
chmod 700 .beads
git config beads.role maintainer

# Not used here.
bd setup cursor --remove
git rm .claude/settings.json    # a second `bd prime` hook; the global one already covers it

bd dolt remote add origin git+https://github.com/tomasz-feliksik/worktree-keeper.git --allow-git-origin
bd config set sync.remote "https://github.com/tomasz-feliksik/worktree-keeper.git"
```

`bd init` commits its own files. Issues then sync with `bd dolt push`, which writes `refs/dolt/data` on the remote and
also creates a `__dolt_remote_info__` branch there. That branch is normal bookkeeping.

**Push `main` before the first `bd dolt push`.** On an empty GitHub repository, the first branch to arrive becomes the
default.

Git pushes do not carry the tracker, so finishing a session takes two pushes: `bd dolt push` and `git push`.

## 3. Add the structure

```text
flake.nix               inputs, plus one line handing over to modules/
rust-toolchain.toml     the pinned toolchain, read by fenix and by rustup
Cargo.toml, Cargo.lock
deny.toml               dependency licence and advisory policy
justfile                fmt, lint, test, check
.markdownlint.yaml
.envrc                  use flake
modules/
  parts.nix             systems and flake modules, nothing else
  package.nix           package, checks, dev shell
  formatter.nix         treefmt
  home.nix              the Home Manager module
src/
docs/
```

Every `.nix` file under `modules/` is a flake-parts module found automatically by `import-tree`. **Git must track a file
for the flake to see it**: an untracked file is silently absent, so run `git add` before building.

### `flake.nix`

There is deliberately no `nix-lib` input, so that the flake still builds for others if this repository goes public.

```nix
{
  description = "worktree-keeper: keeps agent git worktrees safe to create, inspect and remove";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-parts = {
      url = "github:hercules-ci/flake-parts";
      inputs.nixpkgs-lib.follows = "nixpkgs";
    };
    import-tree.url = "github:vic/import-tree";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    home-manager = {
      url = "github:nix-community/home-manager";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs = inputs: inputs.flake-parts.lib.mkFlake {inherit inputs;} (inputs.import-tree ./modules);
}
```

### `modules/parts.nix`

`x86_64-darwin` is left out because Nixpkgs 26.11 dropped it, and evaluating it now fails.

```nix
{inputs, ...}: {
  imports = [
    inputs.home-manager.flakeModules.home-manager
    inputs.treefmt-nix.flakeModule
  ];
  systems = ["x86_64-linux" "aarch64-linux" "aarch64-darwin"];
}
```

### `rust-toolchain.toml` and `Cargo.toml`

```toml
[toolchain]
channel = "1.99.0"
components = ["clippy", "rustfmt", "rust-src", "rust-analyzer"]
```

```toml
[package]
name = "worktree-keeper"
version = "0.0.0"
edition = "2024"
rust-version = "1.99"
publish = false

[[bin]]
name = "worktree"
path = "src/main.rs"

[dependencies]
```

`publish = false` stays until a licence is chosen.

### `modules/package.nix`

The `sha256` belongs to the toolchain channel. After changing the channel, build once with `pkgs.lib.fakeSha256`, then
copy the hash from the `got:` line of the error.

```nix
{inputs, ...}: {
  perSystem = {
    pkgs,
    self',
    system,
    ...
  }: let
    toolchain = inputs.fenix.packages.${system}.fromToolchainFile {
      file = ../rust-toolchain.toml;
      sha256 = "sha256-zm3dyIY2T414ZRR3EhLOvptzG6gta4WZUcawzMUWtqI=";
    };
    craneLib = (inputs.crane.mkLib pkgs).overrideToolchain toolchain;
    src = craneLib.cleanCargoSource ../.;
    commonArgs = {
      inherit src;
      strictDeps = true;
    };
    cargoArtifacts = craneLib.buildDepsOnly commonArgs;
  in {
    packages.default = craneLib.buildPackage (commonArgs
      // {
        inherit cargoArtifacts;
        meta.mainProgram = "worktree";
      });
    checks = {
      package = self'.packages.default;
      clippy = craneLib.cargoClippy (commonArgs
        // {
          inherit cargoArtifacts;
          cargoClippyExtraArgs = "--all-targets -- --deny warnings";
        });
    };
    devShells.default = pkgs.mkShell {
      packages = [toolchain] ++ (with pkgs; [just cargo-deny cargo-audit cargo-mutants git direnv lsof markdownlint-cli2]);
    };
  };
}
```

The dev shell provides the pinned toolchain with `rust-analyzer`, `just`, the two audit tools, `cargo-mutants`, and the
programs the tool needs at runtime or in tests. Formatters are not listed, because `nix fmt` provides them.

### `modules/formatter.nix`

```nix
{
  perSystem.treefmt = {
    projectRootFile = "flake.nix";
    programs = {
      alejandra.enable = true;
      rustfmt = {
        enable = true;
        edition = "2024";
      };
      prettier = {
        enable = true;
        settings = {
          printWidth = 120;
          proseWrap = "always";
        };
      };
    };
  };
}
```

### `justfile`

Recipes assume the dev shell is active and never call `nix develop` themselves.

```just
default:
    @just --list

fmt:
    nix fmt

fmt-check:
    nix fmt -- --ci

clippy:
    cargo clippy --all-targets -- --deny warnings

audit:
    cargo audit

deny:
    cargo deny check

markdownlint:
    markdownlint-cli2 "**/*.md" "#.beads" "#.agents" "#target"

lint: fmt-check clippy audit deny markdownlint

test:
    cargo test

mutants:
    cargo mutants

check: lint test
```

### Lint configuration

`deny.toml`:

```toml
[licenses]
allow = ["MIT", "Apache-2.0", "Unicode-3.0"]
private = { ignore = true }

[bans]
multiple-versions = "warn"
```

`.markdownlint.yaml`:

```yaml
MD013:
  line_length: 120
  tables: false
  code_blocks: false
MD029:
  style: one
```

Add `target/`, `.direnv/` and `result` to `.gitignore`.

### The Home Manager module

`modules/home.nix` publishes `homeModules.default`. It carries an explicit `key`, so that importing it from two places
does not declare its options twice. A consumer writes:

```nix
{
  imports = [inputs.worktree-keeper.homeModules.default];
  worktreeKeeper.root = "${config.home.homeDirectory}/worktrees";
}
```

`root` has no default, because a default would silently put worktrees somewhere nobody chose.

The module should bake `WORKTREE_ROOT` in with `wrapProgram --set-default`, as `ai-harness` does today, rather than
using `home.sessionVariables`. A session variable does not reach hooks and agents started outside a login shell. The
scratch test used a session variable, so the wrapper form still has to be tested here.

## Clone it on another machine

```bash
git clone git@github.com:tomasz-feliksik/worktree-keeper.git ~/code/projects/worktree-keeper
cd ~/code/projects/worktree-keeper
bd init --non-interactive    # restores issues from refs/dolt/data
git status                   # bd may rewrite its generated files; restore anything you did not intend to change
bd stats                     # check the issue count
direnv allow                 # once the flake exists
```

This restore path has not been tested on this repository yet.

## Verify

Check by exit status, not by reading the end of the output.

```bash
nix develop --command just fmt
nix develop --command just check; echo "rc=$?"
nix flake check; echo "rc=$?"
nix build .#default && ./result/bin/worktree
```

For the tracker:

```bash
git ls-remote origin 'refs/dolt/*'    # after the first `bd dolt push`, prints refs/dolt/data
bd dolt remote list
```
