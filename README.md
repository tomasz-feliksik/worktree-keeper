# worktree-keeper

Keeps agent git worktrees safe to create, inspect and remove. It refuses to delete anything it cannot prove is
disposable, including commits that exist only in a worktree's own submodule clones.

**Status: in production.** `ai-harness` takes it from a release tag, in place of the Bash tool it was ported from.

- [Project overview](./docs/project-overview.md): what it is, what it adds beyond Git, and what has been agreed
- [Setup](./docs/setup.md): creating and cloning the repository, and its Nix structure
- [Porting guide](./docs/porting-guide.md): what it kept from the Bash tool, and what it must keep doing
- [JSON schema](./docs/json-schema.md): the output of `status --json` and `check --json`

## Licence

[MIT](./LICENSE)
