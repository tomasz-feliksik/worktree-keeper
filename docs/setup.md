# Working on the repository

How to get a working checkout, how the repository is laid out, and how to release it. What the project is and why these
choices were made: [`project-overview.md`](./project-overview.md).

## Get a checkout

Needs Nix, direnv and `bd` 1.0 or newer.

```bash
git clone git@github.com:tomasz-feliksik/worktree-keeper.git
cd worktree-keeper
bd init --non-interactive    # restores issues from refs/dolt/data
git status                   # bd may rewrite its generated files; restore anything you did not intend to change
bd stats                     # check the issue count
direnv allow
```

This restore path has not been tested on this repository yet. Do not run `bd init` from a linked worktree: it walks up
to `~/.beads` and builds the tracker in the wrong place.

## Check your work

Check by exit status, not by reading the end of the output.

```bash
nix develop --command just fmt
nix develop --command just check; echo "rc=$?"    # lint and every test: what CI runs
nix flake check; echo "rc=$?"                     # builds, Clippy, formatting, Home Manager module
nix build .#default && ./result/bin/worktree --version
```

## The structure

The files themselves are the reference, so they are not repeated here.

```text
flake.nix               inputs, plus one line handing over to modules/
rust-toolchain.toml     for rustup users; the Nix build takes its toolchain from fenix
Cargo.toml, Cargo.lock
deny.toml               dependency licence and advisory policy
justfile                fmt, lint, test, check, black-box, mutants
.markdownlint.yaml
.envrc                  use flake
.github/workflows/      CI: format_check, lint, test, and release_check on tags
modules/
  parts.nix             systems and flake modules, nothing else
  package.nix           package, checks, dev shell
  formatter.nix         treefmt, and what it must not touch
  home.nix              the Home Manager module and its check
src/, tests/cli.rs      the CLI surface and its exit contract
tests/json.rs           the --json contract
tests/black_box/        the black-box suite: see internals.md
docs/
```

Every `.nix` file under `modules/` is a flake-parts module found automatically by `import-tree`. **Git must track a file
for the flake to see it**: an untracked file is silently absent, so run `git add` before building.

### Choices worth knowing

- **No `nix-lib`**, so that the flake builds for anyone: this repository is public.
- **fenix `stable`, not `fromToolchainFile`.** The latter fetches its manifest during evaluation, which is
  import-from-derivation. `ai-harness` evaluates with that disabled, so it could not have built this package. `stable`
  is pinned by `flake.lock`, so moving the toolchain means `nix flake update fenix`.
- **Three platforms.** `x86_64-darwin` is left out because Nixpkgs 26.11 dropped it, and evaluating it now fails. CI
  builds on `x86_64-linux` only.
- **Formatters are not in the dev shell**, because `nix fmt` provides them. It leaves alone the files `bd` generates.
- **Actions in CI are pinned by commit**, with the version in a comment.

### The Home Manager module

`homeModules.default` carries an explicit `key`, so importing it from two places does not declare its options twice. A
consumer writes:

```nix
{
  imports = [inputs.worktree-keeper.homeModules.default];
}
```

There is nothing to configure. Other modules reach the command through `worktreeKeeper.package`, by store path, since a
hook run by a long-lived server cannot rely on `PATH`. `checks.home-module` builds a test home and runs the command from
it.

## The tracker

Issues live in a local Dolt database and sync to `refs/dolt/data` on the same remote. That ref is public, like
everything else here. Git pushes do not carry it, so finishing a session takes two pushes: `bd dolt push` and
`git push`. The `__dolt_remote_info__` branch on the remote is normal bookkeeping, and CI skips it.

```bash
git ls-remote origin 'refs/dolt/*'    # prints refs/dolt/data
bd dolt remote list
```

## Releasing

Consumers build from source, so a release is a version in `Cargo.toml` and a tag on a commit CI has passed. There are no
binaries to upload. A change to the docs or the tests alone needs no release.

1. On a branch, set `version` in `Cargo.toml`, and let `cargo build` update `Cargo.lock`. Merge it.
1. Run `nix develop --command just mutants` on that commit, and explain or kill every survivor.
1. Tag it and push the tag: `git tag -a vX.Y.Z -m "vX.Y.Z"`, then `git push origin vX.Y.Z`.
1. CI runs every job again on the tag, and `release_check` fails if the tag differs from what the binary reports. If it
   fails, leave that tag alone and release the next version.
1. In a consumer, pin the tag: `github:tomasz-feliksik/worktree-keeper/vX.Y.Z`.

Before 1.0, a change that breaks a caller raises the minor version, and anything else raises the patch version. Breaking
means a guarantee, a command-line option, an exit code, or the JSON schema: see [the schema's rules](json-schema.md).
