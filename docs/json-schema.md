# The `--json` schema

`worktree check --json`, `worktree status --json` and `worktree move --json` are the agent interface: an agent acts on a
verdict instead of parsing Git output. This is a public contract from the first release. Until then it can still change.

## Versioning

Every document carries an integer `schema_version`, currently `1`.

- **Adding** a field, or a new value of `kind`, does not change the version. Consumers must ignore fields they do not
  know, and must treat a `kind` they do not know as a refusal.
- **Removing** or renaming a field, or changing what one means, adds one to the version.

Exit statuses do not change with `--json`, so a caller can use either. Prose stays on stderr; stdout holds exactly one
JSON document. A path that is not valid UTF-8 is shown lossily, so never pass a path from this output back to a command
if it holds U+FFFD. A usage error (exit 2) is reported by `clap` and produces no document.

## `verdict`

| Value       | Means                                             | `check` exits |
| ----------- | ------------------------------------------------- | ------------- |
| `removable` | Every check ran, and every check passed           | 0             |
| `refused`   | A check ran, and the answer is no                 | 1             |
| `unknown`   | A check could not run. **Never treat it as safe** | 3             |

## `check --json <path>`

```json
{
  "schema_version": 1,
  "path": "/home/me/worktrees/repo/feat-x",
  "verdict": "refused",
  "reasons": [{ "kind": "dirty", "message": "… has uncommitted changes …" }]
}
```

`check` stops at the first thing it finds, as `guard` does, so `reasons` holds one entry when the verdict is not
`removable`, and none when it is. It is an array so that reporting several later is not a breaking change. `message` is
for people; act on `kind`.

| `kind`                 | Means                                                                         |
| ---------------------- | ----------------------------------------------------------------------------- |
| `not_a_directory`      | The path is not a directory                                                   |
| `main_worktree`        | The path is the main checkout, which is never removable                       |
| `locked`               | `git worktree lock` holds it                                                  |
| `in_use`               | A process has its working directory inside it                                 |
| `dirty`                | It has uncommitted changes                                                    |
| `detached_commits`     | Its detached HEAD holds commits no branch, tag or remote does                 |
| `submodule_pointer`    | A submodule is checked out or staged at a commit other than the one recorded  |
| `dirty_submodule`      | A submodule has uncommitted changes. `--force` does not cover these           |
| `unpushed_submodule`   | A submodule clone holds commits no remote has. `--force` does not cover these |
| `stranded_differs`     | A stranded submodule's files differ from its recorded commit                  |
| `stranded_untraceable` | A stranded submodule cannot be traced to a clone                              |
| `still_populated`      | A submodule would still be populated after emptying                           |
| `unmerged_branch`      | `status` only, and `remove`: its branch is not merged                         |
| `destination_exists`   | `move` only: something is already at the destination                          |
| `orphan`               | `status` only: its `.git` file leads nowhere                                  |
| `could_not_check`      | Verdict `unknown`: a check could not run                                      |
| `other`                | Anything else that refused                                                    |

## `status --json [path]`

```json
{
  "schema_version": 1,
  "worktrees": [
    {
      "path": "/home/me/worktrees/repo/feat-x",
      "repo": "/home/me/code/repo",
      "branch": "feat/x",
      "dirty": false,
      "unpushed": false,
      "merged": true,
      "locked": null,
      "in_use": [{ "pid": 4242, "name": "claude" }],
      "verdict": "refused",
      "reasons": [{ "kind": "in_use", "message": "…" }]
    }
  ]
}
```

| Field      | Type                         | `null` means                                                     |
| ---------- | ---------------------------- | ---------------------------------------------------------------- |
| `path`     | string                       | Never null                                                       |
| `repo`     | string or null               | Could not be read                                                |
| `branch`   | string or null               | Detached, or could not be read                                   |
| `dirty`    | boolean or null              | Could not be read                                                |
| `unpushed` | boolean or null              | Could not be read                                                |
| `merged`   | boolean or null              | Could not be read, or there is no branch                         |
| `locked`   | string or null               | Not locked. A string is the lock's reason, and can be empty      |
| `in_use`   | array or null                | Could not be read. An empty array means nothing is running there |
| `verdict`  | as above                     | Never null. Any field that could not be read makes it `unknown`  |
| `reasons`  | array of `{ kind, message }` | Never null                                                       |

`merged` is true for a branch `worktree remove` would delete without `--force`: one that `git branch -d` accepts, or one
with nothing its target lacks, because it was squashed or rebased.

The verdict answers "would `worktree remove`, with no options, succeed?", so unlike `check` it counts an unmerged branch
as a refusal. The default branch is the exception: `remove` keeps it, so whether it is merged decides nothing. `status`
exits 0 if every worktree could be read, and 3 if any is `unknown`. A refusal is a normal answer to a question about
many worktrees, so it does not change the exit status.

## `move --json <path> [destination]`

```json
{
  "schema_version": 1,
  "moved": {
    "from": "/home/me/code/worktrees/repo/feat-x",
    "to": "/home/me/code/repo.worktrees/feat-x",
    "stale": [{ "entry": ".venv/", "file": ".venv/bin/pytest" }]
  },
  "reasons": []
}
```

A caller that follows a move with steps of its own, such as moving an agent's history, chains them on `moved`.

`stale` lists what git ignores, in the worktree and its submodules, that still names the old absolute path: each entry
as `git ls-files --others --ignored --directory` gives it, and the first file in it, by name, that holds the path. Both
are relative to the new path. `move` changed none of them and knows nothing of what made them, so remaking them (a
virtualenv, a build directory, a path-keyed approval) is the caller's step. An empty array means none does.

| `moved`            | `reasons` | Exit | Means                                                                                                                                                          |
| ------------------ | --------- | ---- | -------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `{ "from", "to" }` | empty     | 0    | Moved, and every check after the move matched the snapshot before it                                                                                           |
| `null`             | empty     | 0    | Already there; nothing changed                                                                                                                                 |
| `null`             | one entry | 1    | Refused before anything changed: `main_worktree`, `locked`, `in_use`, `destination_exists`, …                                                                  |
| `null`             | one entry | 3    | `could_not_check`. Before the move: nothing changed. After it: the worktree may be at the new path, and the message names it and where both snapshots are kept |
