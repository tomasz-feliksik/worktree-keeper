# worktree-keeper

A Rust CLI (binary `worktree`) that makes agent git worktrees safe to create, inspect and remove. **The commands are a
skeleton so far**: each one exits 3. The working implementation is the frozen Bash tool in `reference/bash/`.

## Start here

1. `bd ready` for what can be done next. The work is in two epics: `worktree-keeper-elj` and `worktree-keeper-6fl`.
1. `docs/project-overview.md` for what this is, the agreed scope and every decision. Read it before proposing features.
1. `docs/porting-guide.md` before porting any command. It has what the code cannot tell you: callers, Git facts and
   the guarantee table.
1. `docs/setup.md` for the repository and Nix structure.

## Commands

```bash
nix develop --command just fmt       # format
nix develop --command just check     # lint and test: the CI entry point
nix develop --command just parity    # black-box suite; fails until the port is complete
nix flake check                      # builds, Clippy, formatting, Home Manager module
```

## Rules for this repository

- **The guarantees in the docs are the product.** Never weaken one to make a test or a workflow easier. Show that each new
  test fails when its guarantee is broken.
- **"Could not check" is never "safe".** It exits 3. Do not let it turn into success or an empty result.
- **No `nix-lib`.** It is private, and this flake has to build for others if the repository goes public. This is a
  deliberate exception to the usual project setup.
- **No import-from-derivation.** `ai-harness` evaluates this flake with it disabled and with warnings as errors.
- **Runtime crates stay at `clap`, `serde` and `serde_json`.** Run `git` as a program, not through `git2` or `gix`.
- **Keep it independent of its consumers.** No knowledge of `bd` or `ai-harness` belongs in the tool. The one exception,
  `WM_WORKTREE_PATH`, is explained in the porting guide.
- **Docs stay publishable:** no private paths or another repository's bead ids.
- **`reference/bash/` is frozen.** Read it, do not edit it. It is deleted after cutover.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:cdcea925 -->
## Beads Issue Tracker

This project uses **bd (beads)** for issue tracking. Run `bd prime` to see full workflow context and commands.

### Quick Reference

```bash
bd ready              # Find available work
bd show <id>          # View issue details
bd update <id> --claim  # Claim work
bd close <id>         # Complete work
```

### Rules

- Use `bd` for ALL task tracking — do NOT use TodoWrite, TaskCreate, or markdown TODO lists
- Run `bd prime` for detailed command reference and session close protocol
- Use `bd remember` for durable project facts — not per-tool memory files, which fragment across accounts; keep per-operator preferences in your harness's own memory

**Architecture in one line:** issues live in a local Dolt DB; sync uses `refs/dolt/data` on your git remote; `.beads/issues.jsonl` is a passive export. See https://github.com/gastownhall/beads/blob/main/docs/core-concepts/sync-concepts.md for details and anti-patterns.

## Agent Context Profiles

The managed Beads block is task-tracking guidance, not permission to override repository, user, or orchestrator instructions.

- **Conservative (default)**: Use `bd` for task tracking. Do not run git commits, git pushes, or Dolt remote sync unless explicitly asked. At handoff, report changed files, validation, and suggested next commands.
- **Minimal**: Keep tool instruction files as pointers to `bd prime`; use the same conservative git policy unless active instructions say otherwise.
- **Team-maintainer**: Only when the repository explicitly opts in, agents may close beads, run quality gates, commit, and push as part of session close. A current "do not commit" or "do not push" instruction still wins.

## Session Completion

This protocol applies when ending a Beads implementation workflow. It is subordinate to explicit user, repository, and orchestrator instructions.

1. **File issues for remaining work** - Create beads for anything that needs follow-up
2. **Run quality gates** (if code changed) - Tests, linters, builds
3. **Update issue status** - Close finished work, update in-progress items
4. **Handle git/sync by active profile**:
   ```bash
   # Conservative/minimal/default: report status and proposed commands; wait for approval.
   git status

   # Team-maintainer opt-in only, unless current instructions forbid it:
   git pull --rebase
   git push
   git status
   ```
5. **Hand off** - Summarize changes, validation, issue status, and any blocked sync/commit/push step

**Critical rules:**
- Explicit user or orchestrator instructions override this Beads block.
- Do not commit or push without clear authority from the active profile or the current user request.
- If a required sync or push is blocked, stop and report the exact command and error.
<!-- END BEADS INTEGRATION -->


## Build & Test

_Add your build and test commands here_

```bash
# Example:
# npm install
# npm test
```

## Architecture Overview

_Add a brief overview of your project architecture_

## Conventions & Patterns

_Add your project-specific conventions here_
