# Porting guide

Everything needed to port the Bash tool to Rust, from this repository alone. Scope and decisions are in
[`project-overview.md`](./project-overview.md); the work is tracked in `bd`, starting with `bd ready`.

## What to read

| File                         | What it is                                                                                   |
| ---------------------------- | -------------------------------------------------------------------------------------------- |
| `reference/bash/worktree.sh` | The implementation, frozen from `ai-harness` commit `2be0a66`. **Its comments are the spec** |
| `tests/parity/test.sh`       | The black-box tests. They run against any build                                              |
| This document                | What neither file says: callers, Git facts, the guarantee table and how to cut over          |

Read the comments on a function before porting it. Most of them record a failure that was measured on a real repository,
and the code is often shaped around it in ways that look removable.

The first 27 tests came from `ai-harness`, and passed against the Bash build on 2026-11-05. The port added the rest, for
the defects it fixed and the commands it added. The two `bd` wrapper tests stayed in `ai-harness`, because the wrapper
does.

## Running the parity suite

```bash
nix develop --command just parity                     # everything
nix develop --command just parity sweep actor         # by name, without the t_ prefix
```

It builds the debug binary, then runs the suite against it. It uses scratch repositories with local bare remotes, so it
works offline. `just check` runs it, and CI has its own step for it.

`nix flake check` also runs it as `checks.<system>.parity`, in the Nix sandbox, against the wrapped package instead of
the debug binary. That is the only place the package itself gets tested. It then runs `provision` with nothing on
`PATH`, to prove the wrapper supplies `git` and `direnv`. Because the wrapper comes first on `PATH`, a test cannot fake
either program there: make the real one fail instead, as `t_provision_never_fails_its_caller` does.

Mutation testing stays out of the sandbox: it needs hundreds of incremental builds, and its survivors are read rather
than gated on. It runs broken builds of a tool that deletes things as your own user, so `tests/parity/test.sh` keeps
every path inside its scratch directory and clears `WORKTREE_ROOT`.

`just parity-bash` runs the same suite against the frozen Bash tool, to see what the original does. It fails by design:
on `t_help_never_acts`, on every defect fixed below, and on every test of a command Bash does not have.

## Intended differences from Bash

Keep this list current. Anything not listed here must behave exactly as Bash does.

| Behaviour                   | Bash                                                 | Rust                                                        | Why                                     |
| --------------------------- | ---------------------------------------------------- | ----------------------------------------------------------- | --------------------------------------- |
| `--help`                    | Exit 2, usage on stderr                              | Exit 0, help on stdout                                      | `clap`, and the normal convention       |
| Unknown option, any command | Exit 2 or 1, by command, with a hand-written message | Exit 2, `clap`'s message                                    | `clap`; exit 2 means usage anyway       |
| A branch starting with `-`  | Refused by hand                                      | Rejected by `clap` as an option                             | Same result                             |
| No placement root           | The message names `aiHarness.worktree.root`          | The message names `WORKTREE_ROOT` and `worktreeKeeper.root` | The tool should not know its consumers  |
| Paths with spaces           | Split by `awk`, so misread                           | Read with `-z` and split at the tab                         | The limit was never intended            |
| A probe's stderr            | Mixed into some parsed output                        | Never part of what is parsed                                | Parsing a warning is a wrong answer     |
| A failed `git worktree add` | Git's own status, such as 128                        | Exit 1                                                      | The exit contract has no 128            |
| Unreadable directory entry  | A short list                                         | Exit 3                                                      | A short list is what this tool prevents |

The `--help` guarantee stays: **`--help` and a mistyped option never act**, so nothing may be created, and no branch may
be made.

These began as Bash behaviour and were fixed after parity, each as its own commit with a regression test:

| Behaviour                         | Bash                                                            | Rust                                          |
| --------------------------------- | --------------------------------------------------------------- | --------------------------------------------- |
| `create` on a path already in use | Exit 0, printing it, whichever branch is there                  | Exit 1, naming both branches                  |
| `remove` on an unmerged branch    | Removes the worktree, then exit 1                               | Exit 1 first, with nothing changed            |
| `remove` and other registrations  | Prunes the repository, deleting other entries' clones unchecked | Removes only its own                          |
| `remove` and a silent remote      | Reads it as having no such branch, then deletes locally         | Reads it before deleting anything, and exit 3 |
| `provision`, any failing step     | A failed `direnv allow` exits non-zero and fails workmux's hook | Warns and exits 0                             |

**The Bash tool in production still has every one of these until cutover.** The third can lose commits.

## Who calls this tool

These contracts live in `ai-harness`, not here, and nothing in this repository would fail if they broke.

| Caller                | How it calls                                           | What it relies on                                                                                                                                                                        |
| --------------------- | ------------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| workmux `post_create` | `worktree provision`, in the new worktree              | **A failed submodule init must not fail it.** The hook is fail-fast, so a failure leaves a half-created worktree (measured on workmux 0.1.240). Bash does not fully hold this: see below |
| workmux `pre_remove`  | `worktree guard`, in the worktree                      | A non-zero exit aborts removal, even under `workmux remove --force` (measured on 0.1.262). Reads `WM_WORKTREE_PATH`                                                                      |
| herdr create wrapper  | `path=$(worktree create "$branch" "$base")`            | **Stdout is only the path.** Git's output and all messages go to stderr                                                                                                                  |
| herdr remove wrapper  | `worktree guard "$path"`, or `worktree remove "$path"` | Exit status                                                                                                                                                                              |
| The `bd` wrapper      | `worktree actor "$dir"`, with stderr discarded         | Prints the name or nothing, and stays quiet outside Git. It must not fail on a bad directory                                                                                             |

**`provision` and failure.** Every step warns and exits 0, and `t_provision_never_fails_its_caller` holds that. The one
refusal is a path that is not a directory: no real hook passes one, there is no worktree to half-create, and exit 0
would hide a caller's typo.

**`WM_WORKTREE_PATH`.** `guard`'s default path is `$WM_WORKTREE_PATH`, then the current directory. That is the tool's
one piece of knowledge about a specific caller. Keep it for parity, because `t_guard_reads_workmux_env` tests it.
workmux also runs the hook inside the worktree, so it can probably be dropped at cutover. Confirm that first.

**`WORKTREE_ROOT`.** The installed command gets this from its wrapper. An explicit value in the environment wins, and
the tests rely on that.

## In-use detection

Measured on 2026-11-05, against workmux 0.1.278 and tmux 3.8, and by reading the herdr wrapper. herdr itself was not
run.

- **The check runs while panes are open in some callers.** `workmux remove` from outside the window closes the panes
  first, so the hook sees nothing. From inside the window the panes stay open during `pre_remove`, so an agent in a
  sibling pane is reported. The herdr wrapper guards before herdr is asked to close anything, so its panes are always
  reported.
- **So cutover has a decision to make.** The refusal is correct when nothing owns those processes. A caller that closes
  its own panes passes `--ignore-in-use`, which leaves every other check in force. `--force` does not override it, on
  purpose: it accepts losing changes already made, not pulling a directory from under a working process.
- **Only the caller's own chain is exempt:** its process and ancestors. A sibling shares an ancestor with the caller,
  and is exactly what this looks for, so excluding a process group or a whole subtree would defeat it.
- **An unreadable `/proc` entry does not count.** It is normally another user's process: 543 of 1334 on the machine
  measured, so treating it as "cannot tell" would mean always exit 3. Detection instead proves it works by reading its
  own entry, and exits 3 if it cannot.
- **macOS uses `lsof` and has not been run.** Its output parser has a unit test on Linux; the rest of that path is
  untested.

## Git facts the code depends on

Each of these was measured, and each looks wrong enough for someone to "fix" it. Dates and versions are in the comments
in `reference/bash/worktree.sh`.

### Submodule clones

- Each linked worktree has its own clones under `<worktree gitdir>/modules`. A commit made in one exists nowhere else
  until pushed.
- `git worktree remove` refuses on the mere **existence** of that directory, even when empty. `--force` skips the
  refusal and deletes the clones unchecked. So the value of `guard` is the unpushed check, not getting past Git. **Never
  simplify it into `--force`.**
- `git submodule deinit` edits configuration **shared by every worktree**. Run in a linked worktree, it de-registers the
  main checkout's submodules. Empty the directory by hand instead.
- `submodule.recurse=true` does not initialise submodules on `git worktree add`. The test suite runs with it on, because
  real machines do.

### Finding unpushed commits

- The query is `log --oneline --all --not --remotes`. It needs `--all`, not `--branches`: a submodule normally sits on a
  detached HEAD, so its commits are on no branch, and a stash is work too.
- It needs `--work-tree=/`. After a deinit, a nested submodule's git directory keeps `core.worktree` pointing at a
  directory that no longer exists, and Git refuses to run there at all, even for `log`. This was once a silent exit 128.
- With `--work-tree=/`, Git takes the caller's directory as a path prefix, so a bare `ls-tree -r` lists **nothing**. It
  needs `--full-tree`. Missing this once emptied a modified nested submodule unchecked.
- Identify a git directory by its `objects` directory. `logs/` and `refs/` also contain files called `HEAD`.

### Finding submodules

- Use the gitlinks: mode `160000` in `git ls-files -s`. **Not `git submodule status`**, which reports a checked-out
  submodule as uninitialised when `.git/config` lacks its keys.
- "Top-level" means not inside another submodule. It does not mean "no slash in its path": `lib/shared` is top-level.
- A clone lives under the submodule's **name**, not its path. Read the name from `.gitmodules` at the relevant commit.
- Address clones with `--git-dir`, never through the submodule's `.git` file, whose relative path goes stale when a
  repository moves.

### Stranded submodules

A stranded submodule has files and a `.git` file, but the clone it points at is gone, so Git cannot run in it. An older
guard created these, so they have to be recoverable. Its files are compared against the recorded commit, read from any
other clone that holds it, through a throwaway index. Check the main checkout's clone first, then the worktree's own,
which is the only one left once main has retired the submodule. If no clone can answer, exit 3 and say how to get one.

### Pointers and removal

- Under `worktree.useRelativePaths`, each pointer is relative to the directory **holding it**, never the current
  directory: `<worktree>/.git` from the worktree, and `<admin dir>/gitdir` from the admin directory. Getting this wrong
  made `sweep --apply` unregister live worktrees.
- `git worktree prune` works on the whole repository and deletes each entry's clones. So `sweep` judges each
  registration alone, deletes a clean one's admin directory itself, and leaves locked ones alone, and `remove` does not
  prune at all.
- The worktree is detached before removal, so that `git branch -d` can delete the branch. `git worktree remove` has
  dropped the checkout by then anyway, so a mutant that skips the detach survives; see below.
- A remote branch is deleted last. Deleting it first and then failing locally leaves a branch with no upstream.

### direnv

- Its allow database is keyed by path and content, so a new worktree is never trusted just by existing.
- `direnv status` prints `Found RC allowed 0` on 2.37 and `true` on older versions. Accept both.
- Never use a `whitelist.prefix` on the placement root. It trusts every future `.envrc` under that path, unseen.

### Seed files

- `.worktreeinclude` is read from the main checkout's HEAD. A branch must not be able to widen what it receives, for the
  same reason direnv trust compares bytes.
- A file is copied only if the list selects it **and** git ignores it.
  `ls-files --others --ignored --exclude-from=<list>` answers the first, and the same with `--exclude-standard` answers
  the second.
- `create_new` refuses a symlink only as the last path component. A branch can commit `config` as a symlink, so each
  parent is checked by hand.

## Packaging

The package wraps `git` and `direnv` into its `PATH`, because a hook cannot rely on the caller's, and without `direnv`
the tool would silently skip trust. They are also on `PATH` for tests run by `nix flake check`. macOS also gets `lsof`,
for in-use detection. The Bash tool also needed coreutils, findutils, gawk, sed, grep and diffutils; the Rust port does
not.

## Rust traps this port can avoid or introduce

- **Do not lose Bash's strictness.** `probe()` exits 3 on any failure. Make "could not check" a distinct type, so it
  cannot be mapped to success or to an empty result by accident. A list that comes back short because a probe failed is
  the exact failure this tool exists to prevent.
- **Bash's `git` inherits the caller's environment.** Keep that for configuration. Set `GIT_INDEX_FILE` only on the one
  command that needs it.
- **Paths are bytes.** Use `OsStr` and `Path`, not `String`, for anything from Git or the filesystem. Git quotes unusual
  paths in `ls-files` output by default, so prefer `-z`, which also fixes a limitation in the Bash version.
- **The Bash version's `awk` parsing splits on whitespace.** Do not copy that limit for paths with spaces.
- **`std::process::Command` output is not text.** Decode only what needs decoding, and print paths lossily only for
  humans.

## Guarantee table

Each row is a deliberate break that has to turn the named test red. The first 28 came from `ai-harness`, and the rest
from this port. Every row was re-run against the Rust on 2026-11-05: the source was edited as the row says, and the
named test went red with its own failure message rather than a crash. `cargo-mutants` covers what these rows do not.

| Guarantee broken by the mutant                                 | Test that must fail                             |
| -------------------------------------------------------------- | ----------------------------------------------- |
| Reading a clone without `--work-tree=/`                        | `nested_stale_core_worktree`                    |
| Unpushed check that sees nothing                               | `unpushed_populated_submodule`                  |
| Unpushed check limited to `--branches`                         | `unpushed_detached_submodule`                   |
| Deinitialised clones not inspected                             | `unpushed_leftover_gitdir`                      |
| Worktree's dirty state ignored                                 | `dirty_worktree`                                |
| Submodule's dirty state ignored                                | `dirty_submodule_refused_under_force`           |
| `--help` taken as a branch or path                             | `help_never_acts`                               |
| A failed probe swallowed instead of exit 3                     | `probe_failure_is_loud`                         |
| The main checkout accepted as a worktree                       | `main_worktree_refused`                         |
| "Top-level" read as "no slash in the path"                     | `top_level_submodule_in_subdirectory`           |
| `submodule deinit` used to empty a submodule                   | `remove_keeps_main_submodule_config`            |
| Stranded submodule emptied without comparing it                | `stranded_modified_refused`                     |
| The same, for a submodule main has retired                     | `stranded_retired_modified_refused`             |
| Only the main checkout's clone consulted                       | `stranded_retired_submodule_recovered`          |
| Advising a main checkout that no longer declares the submodule | `stranded_retired_no_clone`                     |
| `ls-tree -r` without `--full-tree`                             | `stranded_modified_refused`                     |
| Guard leaves `<gitdir>/modules` in place                       | `guard_never_removes`                           |
| Actor prints nothing                                           | `actor`                                         |
| Sweep deletes without `--apply`                                | `sweep`                                         |
| Orphans classified as leftovers                                | `sweep`                                         |
| Live worktrees classified as leftovers                         | `sweep_flat_layout`                             |
| Prune ignores unpushed commits                                 | `sweep_stale_registration`                      |
| Registration's `gitdir` read from the current directory        | `sweep_relative_paths`                          |
| A worktree's `.git` pointer read from the current directory    | `sweep_relative_paths`                          |
| An admin directory's back-pointer read raw                     | `sweep_relative_paths`                          |
| direnv trust given when the main checkout is not allowed       | `direnv_gate`                                   |
| direnv trust given when the content differs                    | `direnv_gate`                                   |
| Provision skips submodules                                     | `create_places_and_provisions`                  |
| Unpushed submodule commits accepted under `--force`            | `unpushed_refused_under_force`                  |
| Remote branch deleted without `--delete-remote`                | `remote_branch_deleted_last`                    |
| Remote branch deleted before the local one                     | `remote_branch_deleted_last`                    |
| A silent remote read as having no branch                       | `unreachable_remote_is_could_not_tell`          |
| Sweep prunes a locked registration                             | `sweep_leaves_locked_registrations`             |
| `remove` prunes the whole repository                           | `remove_keeps_other_stale_registrations`        |
| `create` hands back an existing path unchecked                 | `create_refuses_a_slug_collision`               |
| Unmerged branch found only after removal                       | `unmerged_refusal_changes_nothing`              |
| Unmerged check ignores the upstream                            | `unmerged_refusal_changes_nothing`              |
| A failed `direnv allow` fails provision                        | `provision_never_fails_its_caller`              |
| `check` takes optional locks, so it writes the index           | `check_changes_nothing`                         |
| `check` clears what `guard` clears                             | `check_changes_nothing`                         |
| A process inside the worktree goes unnoticed                   | `in_use_refuses_and_names_the_process`          |
| `--force` overrides the in-use check                           | `in_use_refuses_and_names_the_process`          |
| The caller's own chain counts as a holder                      | `in_use_refuses_and_names_the_process`          |
| `--ignore-in-use` ignored                                      | `in_use_refuses_and_names_the_process`          |
| A lock does not stop removal                                   | `lock_refuses_until_unlocked`                   |
| A lock is passed under `--force`                               | `lock_refuses_until_unlocked`                   |
| An orphan listed as safe                                       | `status_lists_every_worktree`                   |
| `status` exits 0 with unknown rows                             | `status_lists_every_worktree`                   |
| A seed file's mode widened                                     | `seed_copies_only_what_is_declared_and_ignored` |
| A selected file copied though git does not ignore it           | `seed_copies_only_what_is_declared_and_ignored` |
| Every ignored file copied, chosen or not                       | `seed_copies_only_what_is_declared_and_ignored` |
| Seeding overwrites                                             | `seed_never_overwrites`                         |
| The list read from the worktree                                | `seed_ignores_a_list_the_branch_changed`        |
| Writing through a symlinked parent                             | `seed_does_not_write_through_a_symlink`         |
| A symlink source copied                                        | `seed_skips_a_symlink_source`                   |
| A failed copy fails the step and the rest                      | `seed_failure_warns_and_exits_zero`             |
| A failed copy leaves a partial file                            | `seed_failure_warns_and_exits_zero`             |
| The seed step's failure fails provision                        | `seed_failure_warns_and_exits_zero`             |

Run mutation testing with `nix develop --command just mutants`.

## Testing a port

For each command: read the Bash and its comments, write or enable the parity tests, port it, then break each of its
guarantees in the Rust and watch the matching test fail. "It passes" is not the claim; "it passes, and it fails like
this when I break it" is.

`cargo-mutants` reports mutants that fail to compile as `unviable`, separately from `caught`. Read those as "not
tested", never as a pass. Here they are generated replacements that have no value of the right type, such as
`Ok(Default::default())` for a type with no default. The functions they target are covered by mutants that did run.

Every survivor is killed or explained here. These are explained rather than killed:

- `paths.rs`, the symlink limit's `>` as `==`: equivalent. The counter goes up by one and returns at the first excess.
- `remove.rs`, the detach's `!` deleted: the detach is redundant today, because `git worktree remove` has dropped the
  checkout before `git branch -d` runs. It stays as a guard against that order changing.

## Cutover

This work happens in `ai-harness`, and needs the decision on how it fetches a private input. Its evaluation disables
import-from-derivation and aborts on warnings. **This flake must stay evaluable under those settings**, which is why it
uses fenix's `stable` rather than `fromToolchainFile`. Test Nix changes from a shell with this set:

```bash
export NIX_CONFIG=$'abort-on-warn = true\nallow-import-from-derivation = false'
nix flake check
```

| In `ai-harness`                                            | Change                                                                                                                                                        |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `flake.nix`                                                | Add this flake as an input, with `follows` for shared inputs                                                                                                  |
| `modules/packages/worktree.nix`                            | Re-export `packages.worktree`, as `usage-ledger.nix` does                                                                                                     |
| `modules/home/worktree.nix`                                | Thin adapter: import `homeModules.default`, map `aiHarness.worktree.root` to `worktreeKeeper.root`, and expose `finalPackage` as `aiHarness.worktree.package` |
| `wrapBeads`, `t_bd_wrapper`, `t_bd_records_worktree_actor` | Stay there, along with the two wrapper mutants                                                                                                                |
| `checks.worktree`, `checks.worktree-mutations`             | Delete with `modules/packages/_worktree/`, and update `checks.output-paths`                                                                                   |
| workmux and herdr unit checks                              | Keep. These are the integration proof                                                                                                                         |
| Its `just standalone-check`                                | Add this input if it is still private                                                                                                                         |
| workmux and herdr hooks                                    | Decide `--ignore-in-use` for callers that close their own panes: see [in-use detection](#in-use-detection)                                                    |

Delete `reference/bash/` here only after a real host has been deployed. Before then, it is the rollback reference.
