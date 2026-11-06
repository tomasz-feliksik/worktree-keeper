# Setting up the repository

How this repository was created, how to clone it, and the Nix and Rust structure it uses. What the project is and why
these choices were made: [`project-overview.md`](./project-overview.md).

All of this has been done in this repository. It is written down so it can be repeated or checked, and so a new machine
can be set up from it.

## 1. Create the repository

Needs `gh`, authenticated, and `bd` 1.0 or newer.

```bash
gh repo create tomasz-feliksik/worktree-keeper --public \
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

## 3. The structure

The files themselves are the reference, so they are not repeated here.

```text
flake.nix               inputs, plus one line handing over to modules/
rust-toolchain.toml     for rustup users; the Nix build takes its toolchain from fenix
Cargo.toml, Cargo.lock
deny.toml               dependency licence and advisory policy
justfile                fmt, lint, test, check, parity, mutants
.markdownlint.yaml
.envrc                  use flake
.github/workflows/      CI: format_check, lint, test
modules/
  parts.nix             systems and flake modules, nothing else
  package.nix           package, checks, dev shell
  formatter.nix         treefmt, and what it must not touch
  home.nix              the Home Manager module and its check
src/, tests/cli.rs      the CLI surface and its exit contract
tests/parity/test.sh    the black-box suite
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

## Releasing

Consumers build from source, so a release is a version in `Cargo.toml` and a tag on a commit CI has passed. There are no
binaries to upload.

1. On a branch, set `version` in `Cargo.toml`, and let `cargo build` update `Cargo.lock`. Merge it.
1. Run `nix develop --command just mutants` on that commit, and explain or kill every survivor.
1. Tag it and push the tag: `git tag -a v0.1.0 -m "v0.1.0"`, then `git push origin v0.1.0`.
1. CI runs every job again on the tag, and `release_check` fails if the tag differs from what the binary reports. If it
   fails, leave that tag alone and release the next version.
1. In a consumer, pin the tag: `github:tomasz-feliksik/worktree-keeper/v0.1.0`.

Before 1.0, a change that breaks a caller raises the minor version, and anything else raises the patch version. Breaking
means a guarantee, a command-line option, an exit code, or the JSON schema: see [the schema's rules](json-schema.md).

## Clone it on another machine

```bash
git clone git@github.com:tomasz-feliksik/worktree-keeper.git ~/code/projects/worktree-keeper
cd ~/code/projects/worktree-keeper
bd init --non-interactive    # restores issues from refs/dolt/data
git status                   # bd may rewrite its generated files; restore anything you did not intend to change
bd stats                     # check the issue count
direnv allow
```

This restore path has not been tested on this repository yet.

## Verify

Check by exit status, not by reading the end of the output.

```bash
nix develop --command just fmt
nix develop --command just check; echo "rc=$?"
nix flake check; echo "rc=$?"
nix build .#default && ./result/bin/worktree --version
```

For the tracker:

```bash
git ls-remote origin 'refs/dolt/*'    # after the first `bd dolt push`, prints refs/dolt/data
bd dolt remote list
```
