# worktree-keeper

Keeps agent git worktrees safe to create, inspect and remove. It refuses to delete anything it cannot prove is
disposable, including commits that exist only in a worktree's own submodule clones.

Each linked worktree has its own submodule clones, so a commit made in one exists nowhere else until it is pushed. Plain
`git worktree remove` refuses whenever those clones exist, and `--force` deletes them without looking. `worktree remove`
looks first.

## Use

```bash
worktree create feat/x            # add ../<repo>.worktrees/feat-x, set it up, print its path
worktree status                   # every worktree of this repository, and whether it could go
worktree check <path>             # may it be removed, and if not, why? Changes nothing
worktree remove <path>            # check, then remove the worktree and its merged branch
worktree sweep                    # list leftovers; deletes only with --apply
```

`worktree --help` lists the rest. `status` and `check` take `--json`, for agents to act on a verdict instead of parsing
Git's output.

| Exit | Meaning                                |
| ---- | -------------------------------------- |
| 0    | Done                                   |
| 1    | Refused, and it says why               |
| 2    | Usage                                  |
| 3    | Could not check. This never means safe |

`--force` accepts losing the worktree's own uncommitted changes and nothing else: an unpushed commit in a submodule is
still refused. `--ignore-in-use` removes while processes are running there, and names each one it left.

## Install

```bash
nix run github:tomasz-feliksik/worktree-keeper -- --help    # try it
cargo install --git https://github.com/tomasz-feliksik/worktree-keeper    # needs git on PATH
```

With Home Manager, import `homeModules.default`: see [Setup](./docs/setup.md#the-home-manager-module).

## Documentation

- [Project overview](./docs/project-overview.md): what it is, what it adds beyond Git, and the decisions behind it
- [Internals](./docs/internals.md): callers, the Git facts behind the code, and how each guarantee is tested
- [JSON schema](./docs/json-schema.md): the output of `status --json` and `check --json`
- [Setup](./docs/setup.md): working on the repository, and releasing it

## Licence

[MIT](./LICENSE)
