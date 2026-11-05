#!/usr/bin/env bash
# Black-box tests for `worktree` — offline, on scratch repositories whose remotes
# are local bare clones. Runs against any build, Bash or Rust:
#
#   WORKTREE=/path/to/bin/worktree bash test.sh [test-name ...]
#
# Exit 0 only if every selected test passed. Each test states the guarantee it
# holds; docs/porting-guide.md lists the mutation that must turn each one red.
set -uo pipefail

: "${WORKTREE:?set WORKTREE to the worktree binary under test}"
SCRATCH=$(mktemp -d)
trap 'rm -rf "$SCRATCH"' EXIT

export HOME=$SCRATCH/home GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=$SCRATCH/gitconfig
mkdir -p "$HOME"
git config --global user.name test
git config --global user.email test@example.invalid
git config --global init.defaultBranch main
git config --global protocol.file.allow always
git config --global advice.detachedHead false
# The machine this ships to sets it globally; the tests must hold under it.
git config --global submodule.recurse true

failures=0
current=""
fail() {
  echo "  FAIL [$current]: $*" >&2
  failures=$((failures + 1))
}

# Run the binary under test; capture status, stdout and stderr separately.
# From a scratch directory, never the caller's: `sweep` also sweeps the
# repository it is run in.
run() {
  out=$(cd "$SCRATCH" && "$WORKTREE" "$@" 2>"$SCRATCH/stderr") && rc=0 || rc=$?
  err=$(cat "$SCRATCH/stderr")
}
expect_rc() {
  [[ $rc -eq $1 ]] || fail "expected exit $1, got $rc — stdout: $out — stderr: $err"
}
expect_out_mention() {
  [[ $out == *"$1"* ]] || fail "stdout does not mention '$1': $out"
}
expect_err() {
  [[ $err == *"$1"* ]] || fail "stderr does not mention '$1': $err"
}

# A project with a submodule that itself has a nested submodule, all pushed to
# local bare remotes. Leaves $T/project checked out on main. The submodule sits
# at $1 (default `sub`); a path with a slash is still a TOP-LEVEL submodule.
make_project() {
  local subpath=${1:-sub}
  T=$SCRATCH/$current
  mkdir -p "$T"
  local r
  for r in nested sub project; do
    git init -q --bare "$T/$r.git"
    git clone -q "$T/$r.git" "$T/$r-seed" 2>/dev/null
    echo "$r" >"$T/$r-seed/README"
    git -C "$T/$r-seed" add README
    git -C "$T/$r-seed" commit -q -m "init $r"
    git -C "$T/$r-seed" push -q origin main 2>/dev/null
  done
  git -C "$T/sub-seed" submodule -q add "$T/nested.git" nested >/dev/null 2>&1
  git -C "$T/sub-seed" commit -q -m "add nested"
  git -C "$T/sub-seed" push -q origin main 2>/dev/null
  git -C "$T/project-seed" submodule -q add "$T/sub.git" "$subpath" >/dev/null 2>&1
  git -C "$T/project-seed" commit -q -m "add sub"
  git -C "$T/project-seed" push -q origin main 2>/dev/null
  git clone -q --recurse-submodules "$T/project.git" "$T/project" 2>/dev/null
}

# A linked worktree of the project, with its submodules initialised the way
# `worktree provision` initialises them.
make_worktree() {
  WT=$T/wt-$1
  git -C "$T/project" worktree add -q -b "$1" "$WT" 2>/dev/null
  git -C "$WT" submodule -q update --init --recursive 2>/dev/null
  WTGIT=$(git -C "$WT" rev-parse --absolute-git-dir)
}

# --- guard / remove -----------------------------------------------------------

# The silent-128 reproduction (worktree-remove.sh, 2026-09-22): after a deinit,
# the NESTED submodule's gitdir keeps core.worktree pointing at a directory that
# no longer exists. Removal must succeed, and must never exit silently.
t_nested_stale_core_worktree() {
  make_project
  make_worktree stale
  git -C "$WT" submodule -q deinit -f --all 2>/dev/null
  local nested_gitdir=$WTGIT/modules/sub/modules/nested
  [[ -d $nested_gitdir ]] || fail "setup: no nested gitdir at $nested_gitdir"
  git --git-dir="$nested_gitdir" log -1 >/dev/null 2>&1 &&
    fail "setup: the nested gitdir is readable without --work-tree — not the failing shape"
  run remove "$WT"
  expect_rc 0
  [[ ! -e $WT ]] || fail "worktree directory still exists"
}

# An unpushed commit in a POPULATED submodule is refused, and named.
t_unpushed_populated_submodule() {
  make_project
  make_worktree pop
  git -C "$WT/sub" checkout -q -b work
  echo change >>"$WT/sub/README"
  git -C "$WT/sub" commit -q -am "unpushed in sub"
  run guard "$WT"
  expect_rc 1
  expect_err "holds commits no remote has"
  expect_err "modules/sub"
  [[ -e $WT/sub/.git ]] || fail "guard deinitialised a submodule it refused to clear"
}

# --force covers only the worktree's own uncommitted changes: an unpushed
# submodule commit is refused under it, on remove and on guard alike.
t_unpushed_refused_under_force() {
  make_project
  make_worktree forced
  git -C "$WT/sub" checkout -q -b work
  echo change >>"$WT/sub/README"
  git -C "$WT/sub" commit -q -am "unpushed in sub"
  run guard --force "$WT"
  expect_rc 1
  expect_err "holds commits no remote has"
  run remove --force "$WT"
  expect_rc 1
  expect_err "holds commits no remote has"
  [[ -e $WT/sub/README ]] || fail "--force emptied a submodule holding an unpushed commit"
}

# Removing one worktree must not cost ANOTHER worktree's clones. `git worktree
# prune` works on the whole repository, so a different registration whose directory
# is gone would lose an unpushed submodule clone that sweep refuses to touch.
t_remove_keeps_other_stale_registrations() {
  make_project
  make_worktree stale-other
  git -C "$WT/sub" checkout -q -b work
  echo change >>"$WT/sub/README"
  git -C "$WT/sub" commit -q -am "unpushed, in the only copy"
  local stale_gitdir=$WTGIT
  rm -rf "$WT"
  make_worktree bystander
  run remove "$WT"
  expect_rc 0
  [[ -d $stale_gitdir/modules/sub ]] || fail "removing another worktree pruned the unpushed clone"
}

# An unpushed commit on a DETACHED head — the normal state of a submodule, and
# on no branch at all — is refused.
t_unpushed_detached_submodule() {
  make_project
  make_worktree det
  echo change >>"$WT/sub/README"
  git -C "$WT/sub" commit -q -am "unpushed, detached"
  run guard "$WT"
  expect_rc 1
  expect_err "holds commits no remote has"
}

# An unpushed commit in a DEINITIALISED submodule's leftover clone is refused.
t_unpushed_leftover_gitdir() {
  make_project
  make_worktree left
  git -C "$WT/sub" checkout -q -b work
  echo change >>"$WT/sub/README"
  git -C "$WT/sub" commit -q -am "unpushed, then deinit"
  git -C "$WT" submodule -q deinit -f --all 2>/dev/null
  [[ -e $WT/sub/.git ]] && fail "setup: sub still populated"
  run guard "$WT"
  expect_rc 1
  expect_err "holds commits no remote has"
  [[ -d $WTGIT/modules ]] || fail "guard cleared modules/ it had refused"
}

# Uncommitted changes in the worktree itself are refused; --force accepts them.
t_dirty_worktree() {
  make_project
  make_worktree dirty
  echo scratch >"$WT/untracked-file"
  run remove "$WT"
  expect_rc 1
  expect_err "uncommitted changes"
  [[ -e $WT ]] || fail "refused removal still deleted the worktree"
  run remove --force "$WT"
  expect_rc 0
  [[ ! -e $WT ]] || fail "--force did not remove the dirty worktree"
}

# Uncommitted changes in a SUBMODULE are refused even with --force.
t_dirty_submodule_refused_under_force() {
  make_project
  make_worktree dsub
  echo change >>"$WT/sub/README"
  run remove --force "$WT"
  expect_rc 1
  expect_err "submodule sub"
  [[ -e $WT ]] || fail "worktree deleted despite a dirty submodule"
}

# A probe that cannot run stops with status 3 and names the path — never a
# silent exit. Shape: a submodule clone whose HEAD is garbage.
t_probe_failure_is_loud() {
  make_project
  make_worktree probe
  git -C "$WT" submodule -q deinit -f --all 2>/dev/null
  echo "garbage" >"$WTGIT/modules/sub/HEAD"
  run guard "$WT"
  expect_rc 3
  expect_err "cannot inspect"
  expect_err "modules/sub"
}

t_main_worktree_refused() {
  make_project
  run guard "$T/project"
  expect_rc 1
  expect_err "main worktree"
}

# guard never removes: after it passes, the worktree is still there and still
# registered — only its submodule metadata is gone.
t_guard_never_removes() {
  make_project
  make_worktree keep
  run guard "$WT"
  expect_rc 0
  [[ -d $WT ]] || fail "guard removed the worktree directory"
  git -C "$T/project" worktree list --porcelain | grep -q "^worktree $WT\$" ||
    fail "guard unregistered the worktree"
  [[ ! -d $WTGIT/modules ]] || fail "guard left <gitdir>/modules behind"
  git -C "$T/project" worktree remove "$WT" 2>/dev/null ||
    fail "plain 'git worktree remove' still refuses after guard"
}

# remove deletes a merged branch, refuses to delete an unmerged one.
t_remove_branch_disposition() {
  make_project
  make_worktree merged
  run remove "$WT"
  expect_rc 0
  git -C "$T/project" show-ref --verify --quiet refs/heads/merged &&
    fail "merged branch was not deleted"
  make_worktree unmerged
  echo change >>"$WT/README"
  git -C "$WT" commit -q -am "unmerged work"
  run remove "$WT"
  expect_rc 1
  expect_err "not merged"
  git -C "$T/project" show-ref --verify --quiet refs/heads/unmerged ||
    fail "unmerged branch was deleted without --force"
}

# The remote branch goes last and only on request: kept when the local deletion
# refuses, kept without --delete-remote, deleted with it, from the branch's own
# remote rather than always from origin.
t_remote_branch_deleted_last() {
  make_project
  local has_remote_branch
  has_remote_branch() { git -C "$T/project.git" show-ref --verify --quiet "refs/heads/$1"; }

  make_worktree unmerged
  echo change >>"$WT/README"
  git -C "$WT" commit -q -am "unmerged"
  git -C "$WT" push -q -u origin unmerged 2>/dev/null
  # Merged to nothing: `branch -d` checks HEAD and the upstream, so take the upstream away.
  git -C "$WT" branch --unset-upstream
  run remove --delete-remote "$WT"
  expect_rc 1
  has_remote_branch unmerged || fail "the remote branch went although the local deletion refused"

  make_worktree plain
  git -C "$WT" push -q -u origin plain 2>/dev/null
  run remove "$WT"
  expect_rc 0
  has_remote_branch plain || fail "the remote branch was deleted without --delete-remote"

  make_worktree asked
  git -C "$WT" push -q -u origin asked 2>/dev/null
  run remove --delete-remote "$WT"
  expect_rc 0
  has_remote_branch asked && fail "--delete-remote did not delete the remote branch"
  git -C "$T/project" show-ref --verify --quiet refs/heads/asked && fail "the local branch survived"

  make_worktree absent
  run remove --delete-remote "$WT"
  expect_rc 0
  expect_out_mention "has no branch absent"

  git -C "$T/project" remote add other "$T/project.git"
  make_worktree elsewhere
  git -C "$WT" push -q -u other elsewhere 2>/dev/null
  git -C "$T/project" remote rename origin old-origin
  run remove --delete-remote "$WT"
  expect_rc 0
  has_remote_branch elsewhere && fail "the branch's own remote was not the one asked"
  git -C "$T/project" remote rename old-origin origin
}

# A remote that cannot answer is not a remote with no such branch.
t_unreachable_remote_is_could_not_tell() {
  make_project
  make_worktree away
  git -C "$WT" push -q -u origin away 2>/dev/null
  git -C "$T/project" remote set-url origin "$T/nowhere.git"
  run remove --delete-remote "$WT"
  expect_rc 3
  expect_err "cannot inspect origin"
  git -C "$T/project.git" show-ref --verify --quiet refs/heads/away || fail "the branch left the remote"
}

# A top-level submodule in a SUBDIRECTORY is still top-level. The guard used to
# call a submodule nested whenever its path had a slash, so it deinitialised
# none of `frontend/design-system`, `lib/shared` — then cleared modules/ under
# them, and the worktree could never be removed (2026-09-27).
t_top_level_submodule_in_subdirectory() {
  make_project libs/sub
  make_worktree subdir
  [[ -e $WT/libs/sub/nested/.git ]] || fail "setup: the nested submodule is not populated"
  run remove "$WT"
  expect_rc 0
  [[ ! -e $WT ]] || fail "worktree still exists — stderr: $err"
}

# Removing a worktree must not touch the MAIN checkout. Submodule registration
# (submodule.<name>.url/active/update) lives in the config file every worktree
# shares, so `git submodule deinit` run in a linked worktree de-registers the
# submodules for the main checkout too — measured 2026-09-27 on flow-trackr,
# where it also dropped a local `update = rebase`.
t_remove_keeps_main_submodule_config() {
  make_project libs/sub
  git -C "$T/project" config submodule.libs/sub.update rebase
  local before after
  before=$(git -C "$T/project" config --get-regexp '^submodule\.' | sort)
  [[ $before == *"submodule.libs/sub.url"* ]] || fail "setup: main checkout has no submodule registration"
  make_worktree cfg
  run remove "$WT"
  expect_rc 0
  after=$(git -C "$T/project" config --get-regexp '^submodule\.' | sort)
  [[ $after == "$before" ]] || fail "main checkout's submodule config changed:"$'\n'"before: $before"$'\n'"after:  $after"
}

# Make $WT the shape an old guard left behind: modules/ cleared while the
# submodules stayed populated, each .git file pointing at a gitdir that is gone.
strand_worktree() {
  rm -rf "$WTGIT/modules"
  git -C "$WT/sub" rev-parse --git-dir >/dev/null 2>&1 &&
    fail "setup: the submodule still resolves its gitdir — not stranded"
}

# A stranded worktree whose submodule files match their recorded commits is
# removed: nothing in it exists nowhere else, and the main checkout's clone of
# each submodule is what proves it.
t_stranded_worktree_recovered() {
  make_project
  make_worktree stranded
  strand_worktree
  run remove "$WT"
  expect_rc 0
  [[ ! -e $WT ]] || fail "stranded worktree was not removed — stderr: $err"
}

# ...and one whose submodule files DIFFER from the recorded commit is refused,
# naming the file — at any depth — with nothing deleted.
t_stranded_modified_refused() {
  make_project
  make_worktree strandmod
  echo "only here" >>"$WT/sub/nested/README"
  strand_worktree
  run remove "$WT"
  expect_rc 1
  expect_err "sub/nested"
  expect_err "README"
  [[ -e $WT/sub/nested/README ]] || fail "refused removal still deleted the modified file"
}

# Make $WT's submodule the shape main leaves behind when it RETIRES one — in
# one repository, lib/shared became a flake input (2026-09-28): main
# no longer declares it and its clone under <common>/modules is gone, while the
# worktree still has the submodule and its own clone under <gitdir>/modules.
# The gitfile is made stale the way a repo move makes it — a relative path that
# no longer resolves — so the guard can reach the clone only by --git-dir.
retire_submodule() {
  git -C "$T/project" rm -q sub 2>/dev/null
  git -C "$T/project" rm -qf --ignore-unmatch .gitmodules
  git -C "$T/project" commit -q -m "retire sub"
  git -C "$T/project" config --remove-section submodule.sub 2>/dev/null
  rm -rf "$T/project/.git/modules/sub"
  echo "gitdir: ../../moved-away/.git/worktrees/$(basename "$WT")/modules/sub" >"$WT/sub/.git"
  [[ -d $WTGIT/modules/sub ]] || fail "setup: the worktree's own clone is gone"
  [[ ! -e $T/project/.gitmodules ]] || fail "setup: main still declares the submodule"
}

# A retired submodule whose files match their recorded commit is verified
# against the worktree's OWN clone, and the worktree is removed.
t_stranded_retired_submodule_recovered() {
  make_project
  make_worktree retired
  retire_submodule
  run remove --force "$WT"
  expect_rc 0
  [[ ! -e $WT ]] || fail "worktree with a retired, clean submodule was not removed — stderr: $err"
}

# ...and one with a modified or untracked file is still refused, naming both.
t_stranded_retired_modified_refused() {
  make_project
  make_worktree retmod
  retire_submodule
  echo "only here" >>"$WT/sub/README"
  echo "new" >"$WT/sub/untracked-note"
  run remove --force "$WT"
  expect_rc 1
  expect_err "README"
  expect_err "untracked-note"
  [[ -e $WT/sub/untracked-note ]] || fail "refused removal still deleted the untracked file"
}

# With no clone anywhere, the advice must work although main no longer declares
# the submodule: `submodule update --init` there is a no-op.
t_stranded_retired_no_clone() {
  make_project
  make_worktree retnone
  retire_submodule
  rm -rf "$WTGIT/modules/sub"
  run remove --force "$WT"
  expect_rc 3
  [[ $err != *"submodule update --init"* ]] || fail "advice names 'submodule update --init', which main cannot do: $err"
  expect_err "git clone --bare $T/sub.git"
  [[ -e $WT/sub/README ]] || fail "unverifiable submodule was emptied"
  # And the advice works: run the command it names — once per submodule level,
  # the nested one included — and removal goes through.
  local advised round
  for round in 1 2 3; do
    advised=$(sed -n 's/^ *\(git clone --bare .*\)$/\1/p' <<<"$err")
    [[ -n $advised ]] || break
    eval "$advised -q" 2>/dev/null || fail "the advised command failed: $advised"
    run remove --force "$WT"
    [[ $rc -eq 3 ]] || break
  done
  expect_rc 0
  [[ ! -e $WT ]] || fail "worktree not removed after following the advice — stderr: $err"
}

# workmux runs pre_remove in the worktree and names it in WM_WORKTREE_PATH.
t_guard_reads_workmux_env() {
  make_project
  make_worktree wm
  out=$(cd / && WM_WORKTREE_PATH=$WT "$WORKTREE" guard 2>"$SCRATCH/stderr") && rc=0 || rc=$?
  err=$(cat "$SCRATCH/stderr")
  expect_rc 0
  [[ $out == *"$WT may be removed"* ]] || fail "guard did not act on WM_WORKTREE_PATH: $out"
}

# --- actor --------------------------------------------------------------------

t_actor() {
  make_project
  make_worktree feat-actor
  run actor "$WT"
  expect_rc 0
  [[ $out == "wt-feat-actor" ]] || fail "linked worktree actor: expected wt-feat-actor, got '$out'"
  run actor "$WT/sub"
  [[ $out == "" ]] || fail "a submodule is not a worktree: got '$out'"
  run actor "$T/project"
  [[ -z $out ]] || fail "main checkout must have no actor, got '$out'"
  run actor "$SCRATCH"
  expect_rc 0
  [[ -z $out ]] || fail "outside git must have no actor, got '$out'"
}

# --- provision / create -------------------------------------------------------

t_provision_noops() {
  current_dir=$SCRATCH/$current/bare
  mkdir -p "$current_dir"
  git -C "$current_dir" init -q
  git -C "$current_dir" commit -q --allow-empty -m init
  git -C "$current_dir" worktree add -q "$current_dir-wt" 2>/dev/null
  run provision "$current_dir-wt"
  expect_rc 0
  [[ -z $out ]] || fail "provision said something where nothing applies: $out"
}

t_create_places_and_provisions() {
  make_project
  out=$(cd "$T/project" && WORKTREE_ROOT=$T/root "$WORKTREE" create feat/place 2>"$SCRATCH/stderr") && rc=0 || rc=$?
  err=$(cat "$SCRATCH/stderr")
  expect_rc 0
  [[ $out == "$T/root/project/feat-place" ]] || fail "placed at '$out', expected $T/root/project/feat-place"
  [[ -e $T/root/project/feat-place/sub/nested/.git ]] || fail "create did not provision nested submodules"
}

t_direnv_gate() {
  command -v direnv >/dev/null || {
    fail "direnv not on PATH — the test environment is incomplete"
    return
  }
  export XDG_DATA_HOME=$SCRATCH/$current/xdg XDG_CONFIG_HOME=$SCRATCH/$current/xdgc
  local repo=$SCRATCH/$current/envrepo
  mkdir -p "$repo"
  git -C "$repo" init -q
  echo 'export FOO=1' >"$repo/.envrc"
  git -C "$repo" add .envrc
  git -C "$repo" commit -q -m envrc
  git -C "$repo" worktree add -q "$repo-a" 2>/dev/null
  run provision "$repo-a"
  (cd "$repo-a" && direnv status | grep -Eq '^Found RC allowed (0|true)$') &&
    fail "worktree trusted while the root .envrc was not allowed"
  (cd "$repo" && direnv allow . >/dev/null 2>&1)
  git -C "$repo" worktree add -q "$repo-b" 2>/dev/null
  run provision "$repo-b"
  (cd "$repo-b" && direnv status | grep -Eq '^Found RC allowed (0|true)$') ||
    fail "worktree not trusted although byte-identical to an allowed root"

  # Never further than the root's own trust. A branch whose .envrc differs
  # from the allowed root copy is not trusted: those bytes were never read.
  git -C "$repo" switch -q -c other-envrc
  echo 'export UNREVIEWED=1' >>"$repo/.envrc"
  git -C "$repo" commit -q -am "different envrc"
  git -C "$repo" switch -q -
  git -C "$repo" worktree add -q "$repo-c" other-envrc 2>/dev/null
  run provision "$repo-c"
  (cd "$repo-c" && direnv status | grep -Eq '^Found RC allowed (0|true)$') &&
    fail "worktree trusted although its .envrc differs from the allowed root copy"

  # A root allow gone stale — the root .envrc edited since it was allowed —
  # is no allow at all, even for a worktree matching the root's OLD bytes.
  echo 'export EDITED=1' >>"$repo/.envrc"
  git -C "$repo" worktree add -q "$repo-d" 2>/dev/null
  run provision "$repo-d"
  (cd "$repo-d" && direnv status | grep -Eq '^Found RC allowed (0|true)$') &&
    fail "worktree trusted on a stale root allow"

  # An explicitly denied root is not allowed either.
  git -C "$repo" checkout -q -- .envrc
  (cd "$repo" && direnv deny . >/dev/null 2>&1)
  git -C "$repo" worktree add -q "$repo-e" 2>/dev/null
  run provision "$repo-e"
  (cd "$repo-e" && direnv status | grep -Eq '^Found RC allowed (0|true)$') &&
    fail "worktree trusted although the root .envrc is denied"
  return 0
}

# --- sweep --------------------------------------------------------------------

t_sweep() {
  make_project
  local root=$T/root
  mkdir -p "$root/project/gone-worktree/.vite" "$root/empty-group"
  echo cache >"$root/project/gone-worktree/.vite/x"
  out=$(cd "$T/project" && WORKTREE_ROOT=$root "$WORKTREE" create live 2>/dev/null)
  mkdir -p "$root/project/orphan"
  echo "gitdir: $T/nowhere" >"$root/project/orphan/.git"
  echo "precious" >"$root/project/orphan/work"

  run sweep "$root"
  expect_rc 0
  [[ $out == *"leftover $root/project/gone-worktree"* ]] || fail "leftover not reported: $out"
  [[ $out == *"leftover $root/empty-group"* ]] || fail "empty group not reported: $out"
  [[ $out == *"orphan   $root/project/orphan"* ]] || fail "orphan not reported: $out"
  [[ -d $root/project/gone-worktree ]] || fail "dry run deleted a leftover"

  run sweep --apply "$root"
  expect_rc 0
  [[ ! -e $root/project/gone-worktree ]] || fail "--apply did not delete the leftover"
  [[ ! -e $root/empty-group ]] || fail "--apply did not delete the empty group"
  [[ -f $root/project/orphan/work ]] || fail "--apply deleted an orphan's files"
  [[ -d $root/project/live/.git || -f $root/project/live/.git ]] || fail "--apply touched a live worktree"
}

# The flat layout workmux used before the shared root — <repo>__worktrees/<name>,
# worktrees at depth 1 — is swept the same way when named: the live worktree is
# kept, a leftover beside it is reported.
t_sweep_flat_layout() {
  make_project
  local flat=$T/project__worktrees
  mkdir -p "$flat"
  git -C "$T/project" worktree add -q -b flat-live "$flat/flat-live" 2>/dev/null
  mkdir -p "$flat/old-task/.vite"
  run sweep "$flat"
  expect_rc 0
  [[ $out == *"leftover $flat/old-task"* ]] || fail "flat-layout leftover not reported: $out"
  [[ $out != *"flat-live"* ]] || fail "a live flat-layout worktree was reported: $out"
  run sweep --apply "$flat"
  [[ ! -e $flat/old-task && -d $flat/flat-live ]] || fail "--apply on the flat layout: wrong things deleted"
}

# A registration whose directory is gone, holding an unpushed submodule clone,
# is never pruned.
t_sweep_stale_registration() {
  make_project
  make_worktree stale-reg
  git -C "$WT/sub" checkout -q -b work
  echo change >>"$WT/sub/README"
  git -C "$WT/sub" commit -q -am "unpushed"
  rm -rf "$WT"
  run sweep --apply --repo "$T/project" "$T/empty-root"
  expect_rc 0
  [[ $out == *"NOT pruning"* ]] || fail "stale registration with unpushed clone not reported: $out"
  [[ -d $WTGIT/modules/sub ]] || fail "the unpushed clone was pruned"

  # And one with nothing unpushed IS pruned — even beside the blocked one.
  make_worktree stale-clean
  rm -rf "$WT"
  run sweep --apply --repo "$T/project" "$T/empty-root"
  [[ ! -d $WTGIT ]] || fail "a stale registration with nothing unpushed was not pruned"
}

# A locked registration is left alone even when its directory is gone: it may be
# on removable media, and `git worktree prune` leaves it too.
t_sweep_leaves_locked_registrations() {
  make_project
  make_worktree locked-reg
  git -C "$T/project" worktree lock --reason "on a removable disk" "$WT"
  rm -rf "$WT"
  run sweep --apply --repo "$T/project" "$T/empty-root"
  expect_rc 0
  [[ -d $WTGIT ]] || fail "--apply pruned a locked registration"
  [[ $out != *"pruned"* ]] || fail "reported pruning a locked registration: $out"
}

# With worktree.useRelativePaths both pointers are relative, each to the
# directory holding it: the admin dir's gitdir and the worktree's .git file.
# A live worktree is neither stale nor orphan, wherever sweep runs from, and
# --apply leaves it working; a registration really gone is still pruned.
t_sweep_relative_paths() {
  make_project
  local root=$T/root live=$T/root/project/rel-live gone=$T/root/project/rel-gone
  local admin=$T/project/.git/worktrees
  mkdir -p "$root/project"
  git -C "$T/project" config worktree.useRelativePaths true
  git -C "$T/project" worktree add -q -b rel-live "$live" 2>/dev/null
  git -C "$T/project" worktree add -q -b rel-gone "$gone" 2>/dev/null
  [[ $(cat "$admin/rel-live/gitdir") != /* ]] || fail "setup: gitdir is not relative"
  [[ $(cat "$live/.git") != "gitdir: /"* ]] || fail "setup: .git is not relative"
  rm -rf "$gone"

  run sweep --repo "$T/project" "$root"
  expect_rc 0
  [[ $out != *"rel-live"* ]] || fail "a live worktree was reported: $out"
  [[ $out == *"stale    $gone "* ]] || fail "a gone registration was not reported by its absolute path: $out"

  run sweep --apply --repo "$T/project" "$root"
  expect_rc 0
  [[ $out != *"rel-live"* ]] || fail "--apply reported a live worktree: $out"
  [[ $out == *"pruned   $gone "* ]] || fail "--apply did not report pruning the gone registration: $out"
  [[ ! -e $admin/rel-gone ]] || fail "--apply did not prune the gone registration"
  [[ -d $admin/rel-live ]] || fail "--apply deleted a live worktree's admin dir"
  git -C "$T/project" worktree list | grep -q "rel-live" || fail "--apply unregistered a live worktree"
  git -C "$live" status >/dev/null 2>&1 || fail "--apply cut a live worktree off from its repository"
}

# --help on every subcommand prints usage and acts on nothing, and a mistyped
# option is refused as usage. `create --help` used to take `--help` as the branch
# name; a leading dash is never read as a branch or a path.
t_help_never_acts() {
  make_project
  local sub
  for sub in create provision actor guard remove sweep; do
    out=$(cd "$T/project" && WORKTREE_ROOT=$T/root "$WORKTREE" "$sub" --help 2>"$SCRATCH/stderr") && rc=0 || rc=$?
    err=$(cat "$SCRATCH/stderr")
    expect_rc 0
    [[ $out == *"Usage: worktree $sub"* ]] || fail "$sub --help did not print its usage: $out"
    out=$(cd "$T/project" && WORKTREE_ROOT=$T/root "$WORKTREE" "$sub" --bogus 2>"$SCRATCH/stderr") && rc=0 || rc=$?
    err=$(cat "$SCRATCH/stderr")
    expect_rc 2
    expect_err "--bogus"
  done
  [[ ! -e $T/root ]] || fail "--help or a mistyped option placed something under the root"
  [[ -z $(git -C "$T/project" branch --list -- '-*') ]] || fail "--help or a mistyped option made a branch"
  [[ $(git -C "$T/project" worktree list --porcelain | grep -c '^worktree ') -eq 1 ]] ||
    fail "--help or a mistyped option registered a worktree"
}

# --- runner -------------------------------------------------------------------

all=$(declare -F | awk '{print $3}' | grep '^t_' | sed 's/^t_//')
selected=${*:-$all}
for name in $selected; do
  current=$name
  before=$failures
  "t_$name"
  if [[ $failures -eq $before ]]; then echo "ok   $name"; else echo "FAIL $name"; fi
done
echo "$failures failure(s)"
[[ $failures -eq 0 ]]
