# Porting guide

Everything needed to port the Bash tool to Rust, from this repository alone. Scope and decisions are in
[`project-overview.md`](./project-overview.md); the work is tracked in `bd`, starting with `bd ready`.

## What to read

| File                         | What it is                                                                                   |
| ---------------------------- | -------------------------------------------------------------------------------------------- |
| `reference/bash/worktree.sh` | The implementation, frozen from `ai-harness` commit `2be0a66`. **Its comments are the spec** |
| `tests/parity/test.sh`       | Its 27 black-box tests. They run against any build                                           |
| This document                | What neither file says: callers, Git facts, the guarantee table and how to cut over          |

Read the comments on a function before porting it. Most of them record a failure that was measured on a real repository,
and the code is often shaped around it in ways that look removable.

The copied suite passed 27 of 27 against the Bash build on 2026-11-05. The two `bd` wrapper tests stayed in
`ai-harness`, because the wrapper does.

## Running the parity suite

```bash
nix develop --command just parity                     # everything
nix develop --command just parity sweep actor         # by name, without the t_ prefix
```

It builds the debug binary, then runs the suite against it. It uses scratch repositories with local bare remotes, so it
works offline. It is **not in `just check` yet**, and it fails until the port is complete.

## Intended differences from Bash

Keep this list current. Anything not listed here must behave exactly as Bash does.

| Behaviour                  | Bash                                     | Rust                            | Why                               |
| -------------------------- | ---------------------------------------- | ------------------------------- | --------------------------------- |
| `--help`                   | Exit 2, usage on stderr                  | Exit 0, help on stdout          | `clap`, and the normal convention |
| Unknown option on `create` | Exit 1, `create: unknown option --bogus` | Exit 2, `clap`'s message        | `clap`; exit 2 means usage anyway |
| A branch starting with `-` | Refused by hand                          | Rejected by `clap` as an option | Same result                       |

`t_help_never_acts` asserts the Bash column, so the foundation task updates it. Its guarantee stays: **`--help` and a
mistyped option never act**, so nothing may be created, and no branch may be made.

The two [known defects](./project-overview.md#known-defects-to-fix) are not differences to make during the port. Port
the current behaviour, then fix each one as its own bug with a regression test.

## Who calls this tool

These contracts live in `ai-harness`, not here, and nothing in this repository would fail if they broke.

| Caller                | How it calls                                           | What it relies on                                                                                                          |
| --------------------- | ------------------------------------------------------ | -------------------------------------------------------------------------------------------------------------------------- |
| workmux `post_create` | `worktree provision`, in the new worktree              | **Never exits non-zero.** The hook is fail-fast, so a failure leaves a half-created worktree (measured on workmux 0.1.240) |
| workmux `pre_remove`  | `worktree guard`, in the worktree                      | A non-zero exit aborts removal, even under `workmux remove --force` (measured on 0.1.262). Reads `WM_WORKTREE_PATH`        |
| herdr create wrapper  | `path=$(worktree create "$branch" "$base")`            | **Stdout is only the path.** Git's output and all messages go to stderr                                                    |
| herdr remove wrapper  | `worktree guard "$path"`, or `worktree remove "$path"` | Exit status                                                                                                                |
| The `bd` wrapper      | `worktree actor "$dir"`, with stderr discarded         | Prints the name or nothing, and stays quiet outside Git. It must not fail on a bad directory                               |

**`WM_WORKTREE_PATH`.** `guard`'s default path is `$WM_WORKTREE_PATH`, then the current directory. That is the tool's
one piece of knowledge about a specific caller. Keep it for parity, because `t_guard_reads_workmux_env` tests it.
workmux also runs the hook inside the worktree, so it can probably be dropped at cutover. Confirm that first.

**`WORKTREE_ROOT`.** The installed command gets this from its wrapper. An explicit value in the environment wins, and
the tests rely on that.

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
  registration alone, deletes a clean one's admin directory itself, and leaves locked ones alone.
- The worktree is detached before removal, so that `git branch -d` can delete the branch.
- A remote branch is deleted last. Deleting it first and then failing locally leaves a branch with no upstream.

### direnv

- Its allow database is keyed by path and content, so a new worktree is never trusted just by existing.
- `direnv status` prints `Found RC allowed 0` on 2.37 and `true` on older versions. Accept both.
- Never use a `whitelist.prefix` on the placement root. It trusts every future `.envrc` under that path, unseen.

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

Each row was a deliberate break in `ai-harness` that had to turn the named test red. These 28 are the safety net to
preserve. The needle-replacing mechanism does not carry over: use `cargo-mutants`, and for each row keep a test that was
**shown to fail** when that guarantee is broken.

| Guarantee broken by the mutant                                 | Test that must fail                    |
| -------------------------------------------------------------- | -------------------------------------- |
| Reading a clone without `--work-tree=/`                        | `nested_stale_core_worktree`           |
| Unpushed check that sees nothing                               | `unpushed_populated_submodule`         |
| Unpushed check limited to `--branches`                         | `unpushed_detached_submodule`          |
| Deinitialised clones not inspected                             | `unpushed_leftover_gitdir`             |
| Worktree's dirty state ignored                                 | `dirty_worktree`                       |
| Submodule's dirty state ignored                                | `dirty_submodule_refused_under_force`  |
| `--help` taken as a branch or path                             | `help_never_acts`                      |
| A failed probe swallowed instead of exit 3                     | `probe_failure_is_loud`                |
| The main checkout accepted as a worktree                       | `main_worktree_refused`                |
| "Top-level" read as "no slash in the path"                     | `top_level_submodule_in_subdirectory`  |
| `submodule deinit` used to empty a submodule                   | `remove_keeps_main_submodule_config`   |
| Stranded submodule emptied without comparing it                | `stranded_modified_refused`            |
| The same, for a submodule main has retired                     | `stranded_retired_modified_refused`    |
| Only the main checkout's clone consulted                       | `stranded_retired_submodule_recovered` |
| Advising a main checkout that no longer declares the submodule | `stranded_retired_no_clone`            |
| `ls-tree -r` without `--full-tree`                             | `stranded_modified_refused`            |
| Guard leaves `<gitdir>/modules` in place                       | `guard_never_removes`                  |
| Actor prints nothing                                           | `actor`                                |
| Sweep deletes without `--apply`                                | `sweep`                                |
| Orphans classified as leftovers                                | `sweep`                                |
| Live worktrees classified as leftovers                         | `sweep_flat_layout`                    |
| Prune ignores unpushed commits                                 | `sweep_stale_registration`             |
| Registration's `gitdir` read from the current directory        | `sweep_relative_paths`                 |
| A worktree's `.git` pointer read from the current directory    | `sweep_relative_paths`                 |
| An admin directory's back-pointer read raw                     | `sweep_relative_paths`                 |
| direnv trust given when the main checkout is not allowed       | `direnv_gate`                          |
| direnv trust given when the content differs                    | `direnv_gate`                          |
| Provision skips submodules                                     | `create_places_and_provisions`         |

## Testing a port

For each command: read the Bash and its comments, write or enable the parity tests, port it, then break each of its
guarantees in the Rust and watch the matching test fail. "It passes" is not the claim; "it passes, and it fails like
this when I break it" is.

`cargo-mutants` reports mutants that fail to compile as `unviable`, separately from `caught`. Read those as "not
tested", never as a pass.

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

Delete `reference/bash/` here only after a real host has been deployed. Before then, it is the rollback reference.
