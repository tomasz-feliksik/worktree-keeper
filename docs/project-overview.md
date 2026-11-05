# worktree-keeper: project overview

**worktree-keeper makes agent git worktrees safe to create, inspect and remove.** It is a small Rust command-line tool
that wraps the worktree lifecycle and refuses to delete anything it cannot prove is disposable.

|            |                                                                        |
| ---------- | ---------------------------------------------------------------------- |
| Repository | `tomasz-feliksik/worktree-keeper` on GitHub, public                    |
| Crate      | `worktree-keeper`                                                      |
| Binary     | `worktree`                                                             |
| Status     | Ported and tested. Production still runs the Bash tool in `ai-harness` |

This document records what the project is and what has been agreed. How to create the repository and its Nix structure
is in [`setup.md`](./setup.md). How to do the port is in [`porting-guide.md`](./porting-guide.md).

## Why it exists

Git can do every individual step of working with worktrees. What it does not have is a standard way to run those steps,
a safety check before deleting, or the information an agent workflow needs. That matters more with coding agents: they
run unattended, there are many worktrees at once, and nobody is watching when a `--force` deletes the only copy of a
commit.

The most dangerous case comes from how submodules work in worktrees. Each linked worktree gets its own submodule clones,
stored under its private git directory. A commit made in one of them exists nowhere else until it is pushed, and
removing the worktree deletes it. Git offers two options here, and neither helps:

- **Plain `git worktree remove`** refuses whenever submodule metadata exists, even if everything is safe.
- **`git worktree remove --force`** skips the check and deletes the clones without looking at them.

### What it adds beyond Git

| Task                  | With plain Git                                                                                                  | What worktree-keeper adds                                                                                  | Value          |
| --------------------- | --------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------- | -------------- |
| Create                | `mkdir`, `git worktree add -b`, `git submodule update --init --recursive`                                       | One command and one fixed location                                                                         | Convenience    |
| Trust the environment | Nothing; `direnv allow` is a separate tool                                                                      | Trusts `.envrc` only if it is byte-identical to the main checkout's already-allowed copy                   | Safety         |
| Agent identity        | Nothing                                                                                                         | Derives an identity from the worktree name                                                                 | New capability |
| Check before removal  | No single command; you script the checks across every submodule clone                                           | Refuses on dirty files or commits that exist only in this worktree, and reports exit 3 when it cannot tell | **Main value** |
| Remove                | The two options above                                                                                           | Removes only after the check passes, then handles the branch                                               | **Main value** |
| Clean up              | `git worktree prune` deletes registrations and their clones unchecked, and knows nothing about leftover folders | Dry run by default, judges each entry separately, never deletes orphans                                    | Safety         |

Someone with no submodules, working by hand, may be fine with plain Git and an alias. This tool is for the case where
that stops being true.

## Product boundaries

The product is one promise: **know what is safe, and never lose work.** It sits underneath the tools that launch and
supervise agents (workmux and herdr today), rather than competing with them.

### Features carried over from the Bash tool

| Feature                | What it does                                                                      | Command     |
| ---------------------- | --------------------------------------------------------------------------------- | ----------- |
| Placement              | Puts every worktree at `<parent>/<repo>.worktrees/<slug>`                         | `create`    |
| Branch setup           | Uses an existing branch, or creates one from a base                               | `create`    |
| Submodule provisioning | Initialises submodules; if that fails, warns without failing the caller           | `provision` |
| Trust inheritance      | Applies the `.envrc` rule above                                                   | `provision` |
| Actor identity         | Gives each worktree its own identity, for trackers and similar tools              | `actor`     |
| Removal guard          | Runs the checks, then clears stale submodule metadata                             | `guard`     |
| Guarded removal        | Removes the worktree, deletes a merged branch, and optionally the remote one      | `remove`    |
| Leftover sweep         | Classifies folders as live, orphan, repo or leftover; deletes only with `--apply` | `sweep`     |
| Exit contract          | 0 done, 1 refused, 2 usage, 3 could not check                                     | all         |

### New features in scope

| Feature            | What it does                                                                            | Why                                                                                      |
| ------------------ | --------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------- |
| Status view        | Shows branch, dirty, unpushed, merged and in-use state for each worktree, with `--json` | There is no list today, and "which of these are safe to remove?" is the common complaint |
| Read-only check    | Answers "may this be removed?" and changes nothing                                      | `guard` also empties submodules, so nothing can answer this without side effects         |
| In-use detection   | Refuses removal while a process is running in the worktree                              | Protects a running agent                                                                 |
| Create safety      | Checks that an existing path is really on the requested branch                          | Fixes the slug collision below                                                           |
| Occupancy lock     | Enforces one agent per worktree, using `git worktree lock`                              | The rule is only a convention today, and Git already has the mechanism                   |
| Local file seeding | Copies chosen gitignored files into new worktrees                                       | Covers local settings that Nix and direnv do not provide                                 |

### Out of scope

| Feature                    | Why not                                                                        |
| -------------------------- | ------------------------------------------------------------------------------ |
| Agent launching and tmux   | workmux and herdr do this                                                      |
| Merge flow                 | workmux and Worktrunk do this                                                  |
| Warm worktree pool         | Treehouse's model; unnecessary unless setup time becomes a measured problem    |
| Port and runtime isolation | Worktrunk does this; unnecessary unless port clashes become a measured problem |
| Scheduled sweep            | Ruled out on purpose: an unattended delete is a new way to lose data           |

## Command interface

`+` marks a command the Bash tool does not have. The main design choice is separating questions from actions: today
`guard` does both, which is why you cannot ask whether removal is safe without changing anything.

```text
# Questions: never change anything
worktree status [--json] [path]     + every worktree and its state
worktree check [--json] [--force] [--ignore-in-use] <path>      + may it be removed, and if not, why?
worktree actor [dir]

# Lifecycle
worktree create <branch> [base]
worktree provision [path]
worktree remove [--force] [--ignore-in-use] [--keep-branch] [--delete-remote] <path>
worktree lock [--reason <text>] [path]     + mark as occupied
worktree unlock [path]                     + release it

# Maintenance
worktree sweep [--apply] [--repo <path>]... [<dir>...]

# For hooks (workmux pre_remove)
worktree guard [--force] [--ignore-in-use] [path]     check, then clear metadata
```

`--json` is the agent interface: agents act on a verdict instead of parsing Git output.

### Guarantees that must survive

1. **`--force` covers only the worktree's own uncommitted changes.** Submodule changes and unpushed submodule commits
   are refused regardless.
1. **"Cannot tell" is never "safe".** A check that could not run exits 3 and names the path.
1. **The guard never removes the worktree.**
1. **Orphans are reported, never swept.** A `.git` file whose git directory is gone may sit beside work nothing else
   holds.
1. **Sweep deletes nothing without `--apply`.**
1. **Git's relative pointers resolve from the directory holding them**, never from the current directory. Getting this
   wrong once made `sweep --apply` unregister live worktrees.
1. **`submodule deinit` is never run in a linked worktree.** Its configuration is shared, so it would de-register the
   main checkout's submodules.
1. **A remote branch is deleted last, and only on request.**

## Defects fixed since the Bash tool

The port first matched Bash, then fixed each of these as its own commit with a regression test. The
[porting guide](./porting-guide.md#intended-differences-from-bash) lists them beside the other intended differences.

- **Slug collision.** `feat/x` and `feat-x` both become the slug `feat-x`, and `create` used to hand back whichever was
  there. It now refuses a path on another branch.
- **Partial removal.** `remove` used to delete the worktree, then refuse on an unmerged branch. It now checks first.
- **Repository-wide prune.** `remove` used to run `git worktree prune`, which deleted the clones of **other** stale
  registrations unchecked, unpushed commits included. Found during the port. This is real data loss, and the Bash tool
  has it until cutover.
- **`provision` failing its caller.** A failed `direnv allow` used to fail workmux's fail-fast hook, leaving a
  half-created worktree.

## Technical decisions

| Decision             | Choice                                                                              | Why                                                                                                                                    |
| -------------------- | ----------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| Language             | Rust                                                                                | No dynamic typing. A discarded `Result` can be a lint failure, and an exhaustive `match` forces every worktree state to be handled     |
| Runtime crates       | `clap`, `serde`, `serde_json`                                                       | Minimum dependencies; `--json` justifies the last two                                                                                  |
| Git access           | Run the `git` binary, not `git2` or `gix`                                           | Same behaviour and configuration as the user's shell, and no native library                                                            |
| Unsafe code          | `#![forbid(unsafe_code)]`                                                           | Nothing here needs it                                                                                                                  |
| Path handling        | A resolved-path type that can only be built through one function                    | Plain types would not have caught the relative-pointer bug; this design would                                                          |
| Toolchain            | fenix `stable`, pinned by `flake.lock`, plus `rust-toolchain.toml` for rustup users | Same tools as other Rust repositories here. `fromToolchainFile` needs import-from-derivation, which `ai-harness` disables              |
| Nix build            | crane                                                                               | Same pattern as other Rust repositories here                                                                                           |
| `nix-lib`            | **Not used**                                                                        | It is private, so a flake using it could not build for anyone else. This is a recorded exception to the owner's project setup standard |
| Platforms            | `x86_64-linux`, `aarch64-linux`, `aarch64-darwin`                                   | Nixpkgs 26.11 dropped `x86_64-darwin`; evaluating it fails                                                                             |
| Home Manager options | `worktreeKeeper.*`                                                                  | Not `programs.*`, which Home Manager could claim later                                                                                 |
| Name                 | `worktree-keeper`                                                                   | `awt` and `agent-worktree` are both taken by tools in this space                                                                       |
| Binary name          | `worktree`                                                                          | Existing hooks and rules keep working                                                                                                  |
| Licence              | Not chosen, so all rights are reserved until then                                   | The repository is public, and the licence is an open decision                                                                          |
| Tracking             | Full beads database in this repository                                              | Same as other repositories, synced to `refs/dolt/data`                                                                                 |

### Testing

- **Parity:** the Bash tool's `test.sh` drives it as a black box, so the same suite can run against the Rust binary.
  Every intended difference gets written down.
- **Real Git:** tests run real `git` in throwaway repositories, because the bugs that matter come from Git's actual
  on-disk behaviour.
- **Mutation testing:** `cargo-mutants` on the safety code. It reports mutants that fail to compile as `unviable`,
  separately from `caught` (measured on 27.1.0: `4 mutants tested: 1 missed, 2 caught, 1 unviable`), so a mutant that
  never ran cannot pass as a working test. Each guarantee above should also have a test that names it.

### In-use detection

It reads `/proc/<pid>/cwd` on Linux, and runs `lsof` on macOS, whose path has not been run yet. If it cannot run, it
exits 3 rather than guessing. A process inside the worktree refuses removal, `--force` does not override that, and
`--ignore-in-use` does. The measurements, and what they mean for callers, are in the
[porting guide](./porting-guide.md#in-use-detection).

### Locking

`worktree lock` uses `git worktree lock`, and a lock refuses removal under `--force` too, naming its reason. `create`
does not lock on its own: a crashed agent would leave a lock nothing releases, and the in-use check already protects a
running agent without any cleanup.

### Placement

A worktree lives at `<parent>/<repo>.worktrees/<slug>`, beside the main checkout. There is no setting: placement follows
from the repository, so `create`, `status` and `sweep` cannot disagree, and a worktree is where you would look for it.

- **Not inside the checkout.** `git clean -ffdx` there deletes uncommitted work with no guard running, and the directory
  would need ignoring in every repository.
- **Not one shared `.worktrees`.** Every repository in a parent directory would share it, so slugs would collide.
- **Only a checkout has a place.** The parent of a submodule's git directory is inside `.git`, and a bare repository's
  is unrelated, so `create` refuses both.
- **The directory goes when it empties.** `remove` and `sweep --apply` call `rmdir`, so anything inside keeps it. This
  fires only through this tool: a worktree removed by another tool leaves the empty directory for `sweep`.
  `sweep --apply` still deletes a subdirectory it calls a leftover, so look at its dry run first.
- **`worktree.useRelativePaths` is the user's call.** With absolute links, moving the repository and its worktrees
  together breaks them until `git worktree repair`, so `create` says so on stderr. It does not set it: the option
  changes the repository's format version, which older Git refuses to read.

The costs are accepted. Plain `status` and `sweep` cover one repository, `sweep --repo <path>` covers the one named, and
`sweep ~/code/*.worktrees` covers the rest. Renaming a repository strands its worktrees until `git worktree repair`. A
read-only parent directory makes `create` refuse.

### Seed files

A repository lists the gitignored files a new worktree needs in a committed `.worktreeinclude`, in gitignore syntax, and
`provision` copies them. The list is read from the main checkout's HEAD, so a branch cannot widen what it receives. A
file must be both listed and ignored. It is never overwritten, it keeps its mode, and a failure warns.

### Merge detection

`remove` deletes a squash-merged or rebased branch without `--force`, though `git branch -d` calls it unmerged. The test
is whether merging it into its target would leave the target's tree unchanged, so a reverted squash, a conflict or any
commit the target lacks keeps the branch. It deliberately does not use `git cherry`, whose patch ids ignore whitespace,
and it is narrower than Worktrunk's six conditions: when in doubt the branch stays, for `--force` to decide. It compares
with local refs and does not fetch, so a squash merged on a forge counts only once it has been pulled.

The owner does not squash-merge, so this was first deferred. It sits in its own commit, and can be reverted alone.

### JSON

`status --json` and `check --json` are specified in [`json-schema.md`](./json-schema.md).

## How it fits with ai-harness

`ai-harness` is the first consumer. The Bash tool stays in production until the Rust version replaces it.

| Stays in ai-harness                                                  | Moves here                                 |
| -------------------------------------------------------------------- | ------------------------------------------ |
| The `bd` wrapper and its tests; this tool should not know about `bd` | The command and its tests                  |
| workmux and herdr hook wiring, and their unit checks                 | The packaged binary                        |
| `homeModules.worktree` as a thin adapter over `worktreeKeeper.*`     | A Home Manager module with its own options |

The adapter satisfies that repository's rule that its options live under `aiHarness.*`, so its hosts do not change. This
project also supersedes the behaviour-preserving Python rewrite planned there.

## Landscape

Checked 2026-11-04 and 2026-11-05 by reading documentation and source. These tools were **not run**, so the safety gaps
below are unconfirmed by a real test.

|                                       | worktree-keeper                          | Worktrunk                              | Treehouse                               | gtr                                 |
| ------------------------------------- | ---------------------------------------- | -------------------------------------- | --------------------------------------- | ----------------------------------- |
| Language, stars                       | Rust, public                             | Rust, 10.9k                            | Go, 1.9k                                | Bash, 1.8k                          |
| Model                                 | Lifecycle commands called by other tools | Branch-named worktrees plus merge flow | Reusable pool, detached HEAD by default | Per-branch worktrees plus launchers |
| Sets up submodules on create          | Yes                                      | No built-in step found                 | No built-in step found                  | Not mentioned                       |
| Refuses on dirty files                | Yes, including submodules                | Yes, including submodules since 0.80   | Yes                                     | Yes, unless forced                  |
| Refuses on unpushed submodule commits | Yes, tested                              | No general check found                 | No check found                          | No check found                      |
| Detects a worktree in use             | Yes                                      | Not checked                            | Yes, by scanning processes              | Not checked                         |

Worktrunk's PR #4252 fixed silent data loss in `wt remove` caused by submodule ignore settings. It still forces removal
whenever submodules are initialised, and from its source, its dirty check would not see a clean submodule whose commit
was never pushed. That is the gap this project exists to cover, and it is worth confirming with a real test.

Links: [Worktrunk](https://github.com/max-sixty/worktrunk), [Treehouse](https://github.com/kunchenguid/treehouse),
[gtr](https://github.com/coderabbitai/git-worktree-runner).

## Roadmap

1. **Foundation docs.** Done.
1. **Skeleton.** Flake, dev shell, CI and the CLI surface. Done.
1. **Parity.** Port the six commands until the black-box suite passes, with each intentional difference written down.
   Bash stays in production. Done.
1. **Cutover.** Add the input to `ai-harness`, turn its module into the adapter, and delete the Bash tool only once its
   unit checks and a real deployment are green.
1. **Fixes and features.** The known defects, read-only check, status with `--json`, in-use detection, occupancy lock,
   then local file seeding. Done, ahead of cutover.
1. **Later.** Merge detection. Done.

This order is carried by bead priorities, with the port and cutover first. Bead dependencies record only real
constraints, so a fix can land before cutover if there is a reason to.

## Open questions

Each one is a `decision` bead, so `bd list --type=decision` shows the current state. The JSON schema, automatic locking
and seed files were decided on 2026-11-05 as reversible defaults, and await the owner's review while nothing depends on
them yet:

1. Which licence should this repository have?
