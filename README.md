# worktree-keeper

Keeps agent git worktrees safe to create, inspect and remove. It refuses to delete anything it cannot prove is
disposable, including commits that exist only in a worktree's own submodule clones.

**Status: documentation only.** There is no code here yet.

- [Project overview](./docs/project-overview.md): what it is, what it adds beyond Git, and what has been agreed
- [Setup](./docs/setup.md): creating and cloning the repository, and its Nix structure
