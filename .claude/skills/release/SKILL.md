---
name: release
description:
  Cut a worktree-keeper release end to end — find the last tag, diff every surface a caller depends on since (commands
  and options, exit statuses, the --json schema, the guarantees, the Home Manager module), propose the version with the
  evidence shown, raise it in Cargo.toml and Cargo.lock, run every gate including a full mutation run, tag, watch the
  tag's pipeline, and publish a GitHub release with notes written for the people who pin it. Use when asked to release,
  cut or tag a version, or what the next version should be.
---

# release

`docs/setup.md` § Releasing is the procedure and the authority; this skill drives it and owns the parts that need
judgement. If the two disagree, the doc wins and the skill is fixed in the same change.

What a release is here:

- **A version and a tag, nothing to upload.** Consumers build from source and pin
  `github:<owner>/worktree-keeper/vX.Y.Z` (`docs/setup.md`). `Cargo.toml` carries the version, the tag is `vX.Y.Z`, and
  `just check-tag vX.Y.Z` refuses a tag that differs from what `worktree --version` reports.
- **The notes live in the GitHub release.** There is no changelog file. The tag message is the one-line `vX.Y.Z` the
  earlier tags use.
- **CI checks, people publish.** On a `v*` tag CI runs every job again plus `release_check`. The GitHub release is
  created by hand, after those are green.

Three stops need the person's explicit yes: the **version** (Phase 3), the **notes** (Phase 5) and the **push to `main`
and of the tag** (Phase 6). A request that already names them — "cut 0.4.0", "publish it" — is that yes for what it
names, and nothing else. A pushed tag is public and is never moved.

Every non-git command runs through `nix develop --command`, and every gate is judged by its exit status
(`cmd >log 2>&1; rc=$?`), never by a pipeline's last stage.

## Phase 1 — Baseline

```bash
git switch main && git pull --ff-only && git status --short   # clean, up to date
git ls-remote --tags origin 'v*' | sort -t/ -k3 -V | tail -1   # LAST: the newest tag on origin, not git describe
gh release list --limit 3                                     # what has been published
gh run list --branch main --limit 3                           # main is green before anything is cut
```

An empty `LAST..main` range means there is nothing to release: stop. A local `vX.Y.Z` that origin lacks is an earlier
run stopped before the push: read it with `git tag -l --format='%(contents)'`, check it names the commit you mean, and
continue from Phase 6. Never retag.

## Phase 2 — Evidence

```bash
git log --format='%h %s%n%b' LAST..main --no-merges
```

Commit types are pointers, not verdicts: a `fix` can add a refusal a caller has to handle, and a `refactor` can change
an exit status. Diff each surface:

| Surface                                    | Evidence                                                                                                                     |
| ------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------- |
| Commands and options                       | `git diff LAST..main -- src/main.rs tests/cli.rs`; the command list in `docs/project-overview.md`                            |
| Exit statuses                              | 0 done, 1 refused, 2 usage, 3 could not check. `git diff LAST..main -- src/outcome.rs`; any test whose expected `rc` changed |
| The `--json` schema                        | `git diff LAST..main -- docs/json-schema.md tests/json.rs`. Its § Versioning says what is additive                           |
| Refusal kinds                              | `RefusalKind` in `src/outcome.rs`. A new kind is additive: consumers treat an unknown kind as a refusal                      |
| The guarantees                             | `git diff LAST..main -- docs/project-overview.md docs/internals.md`. A guarantee is never weakened; if one moved, stop       |
| What git or the disk is left with          | `src/commands/remove.rs`, `guard.rs`, `sweep.rs`: what a caller finds afterwards, such as a directory left beside a gitdir   |
| Nix: the package and `homeModules.default` | `git diff LAST..main -- modules/ flake.nix flake.lock`. A renamed option or output breaks every configuration using it       |

Collect the beads from the commit bodies (`grep -oE '(Refs|Closes) worktree-keeper-[a-z0-9.]+'`) and `bd show` each: the
title says what the work was for, which is what a reader of the notes wants.

## Phase 3 — Verdict

Classify every surface as **unchanged**, **additive** or **breaking**, each with the diff or commit behind it, then
propose the version. Before 1.0 (`docs/setup.md`):

- anything breaking — a removed or renamed option, a changed exit status, a JSON field removed or redefined, a guarantee
  changed → **minor** (`0.Y.0`)
- a new command → **minor** too, by choice: it is what a consumer bumps for
- fixes, new refusal kinds, new JSON fields, docs and internal changes → **patch** (`0.Y.Z`)
- nothing a caller can see → say so, and ask whether a release is wanted at all; a change to docs or tests alone needs
  none

Versions continue from the tags, never from what `Cargo.toml` happens to say. A version the person names is a claim to
check: if the evidence says breaking and they said patch, stop and show the evidence; if they chose higher, theirs
stands.

**Stop. The person confirms the version before anything changes.**

## Phase 4 — Prepare

File a bead for the release (`Release worktree-keeper X.Y.Z`, type `chore`). A release is one commit, so it goes on
`main` directly; it rides a branch only when it closes that branch's work, as `chore(<id>): release X.Y.Z` there.

1. Set `version` in `Cargo.toml`, and let `nix develop --command cargo build` update `Cargo.lock`.
1. `nix develop --command just check-tag vX.Y.Z`, the check CI runs on the tag.
1. The gates:

   ```bash
   nix develop --command just check
   NIX_CONFIG=$'abort-on-warn = true\nallow-import-from-derivation = false' nix flake check
   nix develop --command just mutants
   ```

   The flake check runs as strict consumers evaluate it. The full mutation run takes minutes: run it in the background,
   and explain or kill every survivor — `docs/internals.md` lists the known equivalent mutants. A new survivor is a
   missing test, and a release is not cut over one. Record each gate's result on the release's bead.

1. Commit `Cargo.toml` and `Cargo.lock` together as `chore(<id>): release X.Y.Z`, with `Closes worktree-keeper-<id>` in
   the body.

## Phase 5 — Notes

Write the notes to a file outside the repository:

- a short opening paragraph: what the release is about;
- **Breaking**, first and only when there is any: what changed, and what a caller must do;
- **New** and **Fixed**: by what a person or an agent will notice, with the why. Name commands, options, exit statuses
  and JSON fields exactly — callers act on those;
- **Upgrading**: the pin line, `github:<owner>/worktree-keeper/vX.Y.Z`, and anything a caller must change;
- **Full changes**: the compare link `https://github.com/<owner>/worktree-keeper/compare/LAST...vX.Y.Z`.

The repository is public and so are the notes: no personal data, no paths or names from the machine the release was cut
on, no other repository's names or issue ids. Examples use invented paths like `/home/me/code/repo`. Notes are not
generated from commits: the log is the evidence, the notes are written for the person bumping the pin.

**Stop. The person approves the notes.**

## Phase 6 — Publish

```bash
git tag -a vX.Y.Z -m "vX.Y.Z"
git describe                     # must print exactly vX.Y.Z
```

**Stop. The person confirms the push**, then:

```bash
bd dolt push
git push origin main && git push origin vX.Y.Z
gh run list --branch vX.Y.Z --limit 1    # then gh run watch <id> --exit-status; release_check must pass
```

Only once the tag's run is green:

```bash
gh release create vX.Y.Z --title "vX.Y.Z" --notes-file <notes> --verify-tag
gh release view vX.Y.Z
```

If `gh release create` fails after the tag is on the remote, fix and re-run the release command only.

## Phase 7 — Report

The version, the release URL, which gates ran where, and each mutation survivor with why it is equivalent. Consumers pin
the tag themselves: name the pin line they change, and offer to bump the ones the person names. Close the release's bead
with the release URL.

## Gotchas

- **Commit `Cargo.lock` with the version.** `check-tag` reads the binary through `cargo run`, which quietly rewrites the
  lock on disk, so the check passes locally while the Nix build reads the committed lock and reports the old version.
- **A red `release_check` on a pushed tag is not fixed by moving the tag.** Leave it, and release the next patch version
  with the fix (`docs/setup.md`).
- **`git push --tags` pushes every local tag**, including throwaway ones. Push the one tag by name.
- **`--no-ff` merges survive only with `rebase.rebaseMerges = true`.** Check `git config --get rebase.rebaseMerges`
  before any `git pull --rebase` on `main`, or the release lands on a flattened history.
