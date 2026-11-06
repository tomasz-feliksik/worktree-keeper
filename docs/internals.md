# Internals

What the code cannot tell you: who calls this tool and what they rely on, the Git facts behind its odd-looking shapes,
and how its guarantees are tested. Read it before changing a command. Scope and decisions are in
[`project-overview.md`](./project-overview.md), and work is tracked in `bd`, starting with `bd ready`.

Read the comments on a function before changing it, too. Most of them record a failure measured on a real repository.

## Who calls this tool

These contracts live in `ai-harness`, not here, and nothing in this repository would fail if they broke.

| Caller                | How it calls                                            | What it relies on                                                                                                                               |
| --------------------- | ------------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------- |
| workmux `post_create` | `worktree provision`, in the new worktree               | **A failed submodule init must not fail it.** The hook is fail-fast, so a failure leaves a half-created worktree (measured on workmux 0.1.240). |
| workmux `pre_remove`  | `worktree guard --ignore-in-use`, in the worktree       | A non-zero exit aborts removal, even under `workmux remove --force` (measured on 0.1.262). Reads `WM_WORKTREE_PATH`                             |
| herdr create wrapper  | `path=$(worktree create "$branch" "$base")`             | **Stdout is only the path.** Git's output and all messages go to stderr                                                                         |
| herdr remove wrapper  | `worktree remove "$path"`; a key adds `--ignore-in-use` | Exit status                                                                                                                                     |
| The `bd` wrapper      | `worktree actor "$dir"`, with stderr discarded          | Prints the name or nothing, and stays quiet outside Git. It must not fail on a bad directory                                                    |

**`provision` and failure.** Every step warns and exits 0, and `provision_never_fails_its_caller` holds that. The one
refusal is a path that is not a directory: no real hook passes one, there is no worktree to half-create, and exit 0
would hide a caller's typo.

**`WM_WORKTREE_PATH`.** `guard`'s default path is `$WM_WORKTREE_PATH`, then the current directory. That is the tool's
one piece of knowledge about a specific caller. `guard_reads_workmux_env` holds it. workmux also runs the hook inside
the worktree, so it can probably be dropped. Nobody has confirmed that yet.

**Placement.** `src/placement.rs` holds the one rule, and every command asks it. Worktrees that already live elsewhere
keep working: `guard` and `remove` take a path, and `status` lists what git has registered, wherever it is.

## In-use detection

Measured on 2026-11-05 against workmux 0.1.278 and tmux 3.8, and on 2026-11-06 against herdr 0.9.5, on an isolated
server through a real client's key bindings.

- **The check runs while panes are open in some callers.** `workmux remove` from outside the window closes the panes
  first, so the hook sees nothing. From inside the window the panes stay open during `pre_remove`, so an agent in a
  sibling pane is reported. A herdr popup binding is a child of the server rather than of the pane, so even the pane's
  own shell is reported, and a plain `guard` or `remove` from a key always refuses.
- **Popups differ from panes in two more ways.** A popup sets `HERDR_ACTIVE_WORKSPACE_ID`, not `HERDR_WORKSPACE_ID`, and
  it closes when its command exits, so a refusal disappears unless the command waits.
- **So a caller decides.** The refusal is correct when nothing owns those processes. A caller that closes its own panes
  passes `--ignore-in-use`, which leaves every other check in force. `--force` does not override it, on purpose: it
  accepts losing changes already made, not pulling a directory from under a working process.
- **The override leaves a record.** `guard` and `remove` with `--ignore-in-use` ask nothing, and print
  `still running: <name> (pid <pid>)` for each process they find. Neither stops a process: that stays with whoever
  closes the panes.
- **Only the caller's own chain is exempt:** its process and ancestors. A sibling shares an ancestor with the caller,
  and is exactly what this looks for, so excluding a process group or a whole subtree would defeat it.
- **An unreadable `/proc` entry does not count.** It is normally another user's process: 543 of 1334 on the machine
  measured, so treating it as "cannot tell" would mean always exit 3. Detection instead proves it works by reading its
  own entry, and exits 3 if it cannot.
- **macOS uses `lsof` and has not been run.** Its output parser has a unit test on Linux; the rest of that path is
  untested.

## Git facts the code depends on

Each of these was measured, and each looks wrong enough for someone to "fix" it. Dates and versions are in the comments
of [the Bash tool](#what-changed-from-the-bash-tool).

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
  dropped the checkout by then anyway, so a mutant that skips the detach survives: see
  [mutation testing](#mutation-testing). If `git worktree remove` itself fails, the worktree is left detached.
- A remote branch is deleted last. Deleting it first and then failing locally leaves a branch with no upstream.

### direnv

- Its allow database is keyed by path and content, so a new worktree is never trusted just by existing.
- `direnv status` prints `Found RC allowed 0` on 2.37 and `true` on older versions. Accept both.
- Never use a `whitelist.prefix` on a worktrees directory. It trusts every future `.envrc` under that path, unseen.

### Merge detection

- `git merge-tree --write-tree` exits 0 on a clean merge and 1 on a conflict. Anything else is "could not check".
- It writes the trees it makes even when asked only a question, so `GIT_OBJECT_DIRECTORY` points it at a scratch
  directory, with the real objects as an alternate. That list splits at colons unless the entry is C-quoted.
- A merge that changes nothing writes no new tree, so only an unintegrated branch shows up an object-store write.
- `git cherry` is not used: its patch ids ignore whitespace, so a branch that only fixed indentation could match a
  commit that did not, and be deleted.

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
for in-use detection.

## Rules the code keeps

- **"Could not check" is its own type.** `probe()` exits 3 on any failure, so it cannot be mapped to success or to an
  empty result by accident. A list that comes back short because a probe failed is the exact failure this tool exists to
  prevent.
- **`git` inherits the caller's environment**, for configuration. Set `GIT_INDEX_FILE` only on the one command that
  needs it.
- **Paths are bytes.** Use `OsStr` and `Path`, not `String`, for anything from Git or the filesystem. Git quotes unusual
  paths in `ls-files` output by default, so prefer `-z`.
- **`std::process::Command` output is not text.** Decode only what needs decoding, and print paths lossily only for
  humans.

## Testing

### The black-box suite

```bash
nix develop --command just black-box                  # everything
nix develop --command just black-box sweep actor      # tests whose names contain either word
```

`tests/black_box/` drives the real binary against real git, in scratch repositories with local bare remotes, so it works
offline. `cargo test`, and so `just check`, runs it too. It reports how many tests ran, and in `just black-box` a word
that matches no test fails rather than passing with none.

`nix flake check` also runs it as `checks.<system>.black-box`, in the Nix sandbox, against the wrapped package instead
of the debug binary, by setting `WORKTREE_UNDER_TEST`. That is the only place the package itself gets tested, and the
package's own build skips the suite, so that a consumer does not have to run it. `checks.<system>.wrapper` then runs
`provision` with nothing on `PATH`, to prove the wrapper supplies `git` and `direnv`. Because the wrapper comes first on
`PATH`, a test cannot fake either program there: make the real one fail instead, as `provision_never_fails_its_caller`
does.

### Guarantee table

Each row is a deliberate break that has to turn the named test red. A new guarantee gets a row, and is shown to fail:
"it passes" is not the claim; "it passes, and it fails like this when I break it" is. The rows were last re-run one by
one on 2026-11-05, when the suite was still shell. It has kept their names, and full mutation runs have been read since.
`cargo-mutants` covers what these rows do not.

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
| Sweep looks beside the caller's repository, not the one named  | `sweep_repo_names_where_to_look`                |
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
| `--ignore-in-use` hides what it left running                   | `ignore_in_use_names_what_it_left_running`      |
| `--ignore-in-use` lets uncommitted changes through             | `ignore_in_use_names_what_it_left_running`      |
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

### Mutation testing

Run it with `nix develop --command just mutants`. It stays out of the sandbox, because it needs hundreds of incremental
builds and its survivors are read rather than gated on. It runs broken builds of a tool that deletes things as your own
user, so `tests/black_box/` keeps every path inside its scratch directory.

`cargo-mutants` reports mutants that fail to compile as `unviable`, separately from `caught`. Read those as "not
tested", never as a pass. Here they are generated replacements that have no value of the right type, such as
`Ok(Default::default())` for a type with no default. The functions they target are covered by mutants that did run.

A full run takes about six minutes. Every survivor is killed or explained here. These are explained rather than killed:

- `paths.rs`, the symlink counter's `+=` as `*=`: a timeout, which is a detection. The counter stays at zero, so a
  symlink cycle never ends, and `cycle` tests hang rather than fail.
- `submodules.rs`, `is_populated` as always true: can time out under the 60s limit. Without the limit it is caught, in
  about two minutes.
- `remove.rs`, the detach's `!` deleted: the detach is redundant today, because `git worktree remove` has dropped the
  checkout before `git branch -d` runs. It stays as a guard against that order changing.
- `provision.rs`, `same_bytes` as always true: survives some full runs and is caught on its own. Tracked as
  `worktree-keeper-elj.4`.

## What consumers need

`ai-harness` fetches this public repository with no credential, and its evaluation disables import-from-derivation and
aborts on warnings. **This flake must stay evaluable under those settings**, which is why it uses fenix's `stable`
rather than `fromToolchainFile`. Test Nix changes from a shell with this set. CI's `test` job sets the same two.

```bash
export NIX_CONFIG=$'abort-on-warn = true\nallow-import-from-derivation = false'
nix flake check
```

The `bd` wrapper and its tests live in `ai-harness`, because this tool knows nothing about `bd`. So do the workmux and
herdr hooks and their unit checks, which are the integration proof. A caller that closes its own panes passes
`--ignore-in-use`, and no other does: see [in-use detection](#in-use-detection).

## What changed from the Bash tool

This tool replaced a Bash script, which was removed once a real host ran the Rust version. Tag `v0.1.2` is the last that
has it, frozen from `ai-harness` commit `2be0a66`. Its comments record the failure behind most of the shapes above, so
read them before removing something that looks unnecessary:

```bash
git show v0.1.2:reference/bash/worktree.sh
```

These defects were fixed, each in its own commit with a regression test:

| Behaviour                         | Bash                                                            | Rust                                          |
| --------------------------------- | --------------------------------------------------------------- | --------------------------------------------- |
| `create` on a path already in use | Exit 0, printing it, whichever branch is there                  | Exit 1, naming both branches                  |
| `remove` on an unmerged branch    | Removes the worktree, then exit 1                               | Exit 1 first, with nothing changed            |
| `remove` and other registrations  | Prunes the repository, deleting other entries' clones unchecked | Removes only its own                          |
| `remove` and a silent remote      | Reads it as having no such branch, then deletes locally         | Reads it before deleting anything, and exit 3 |
| `provision`, any failing step     | A failed `direnv allow` exits non-zero and fails workmux's hook | Warns and exits 0                             |

And these differences were chosen on purpose:

| Behaviour                    | Bash                                                 | Rust                                                          | Why                                          |
| ---------------------------- | ---------------------------------------------------- | ------------------------------------------------------------- | -------------------------------------------- |
| `--help`                     | Exit 2, usage on stderr                              | Exit 0, help on stdout                                        | `clap`, and the normal convention            |
| Unknown option, any command  | Exit 2 or 1, by command, with a hand-written message | Exit 2, `clap`'s message                                      | `clap`; exit 2 means usage anyway            |
| A branch starting with `-`   | Refused by hand                                      | Rejected by `clap` as an option                               | Same result                                  |
| Placement                    | `<root>/<repo>/<slug>`, from a configured root       | `<parent>/<repo>.worktrees/<slug>`, with nothing to configure | See below                                    |
| `status`, `sweep` default    | The configured root, across repositories             | The current repository; refused outside one                   | There is no root any more                    |
| `sweep --repo`, no directory | The configured root                                  | Beside each repository named, from anywhere                   | There is no root any more                    |
| Where `create` can run       | Any repository                                       | Refused unless the git directory is named `.git`              | Elsewhere the path is wrong or inside `.git` |
| Paths with spaces            | Split by `awk`, so misread                           | Read with `-z` and split at the tab                           | The limit was never intended                 |
| A probe's stderr             | Mixed into some parsed output                        | Never part of what is parsed                                  | Parsing a warning is a wrong answer          |
| A failed `git worktree add`  | Git's own status, such as 128                        | Exit 1                                                        | The exit contract has no 128                 |
| Unreadable directory entry   | A short list                                         | Exit 3                                                        | A short list is what this tool prevents      |
