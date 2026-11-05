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
  expect_out_mention "emptied 1 submodule"
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

# "Refused" means nothing happened. The unmerged branch is found before anything is
# deleted: the worktree and its submodule stay. --keep-branch and --force skip it.
t_unmerged_refusal_changes_nothing() {
  make_project
  make_worktree half
  echo change >>"$WT/README"
  git -C "$WT" commit -q -am "unmerged work"
  run remove "$WT"
  expect_rc 1
  expect_err "not merged"
  [[ -f $WT/README ]] || fail "the worktree was removed before refusing"
  [[ -e $WT/sub/README ]] || fail "the submodule was emptied before refusing"
  [[ -d $WTGIT/modules ]] || fail "the submodule clones were cleared before refusing"
  [[ $(git -C "$WT" branch --show-current) == half ]] || fail "the worktree was detached before refusing"

  # git's own rule: merged into the upstream counts, although HEAD lacks it.
  git -C "$WT" push -q -u origin half 2>/dev/null
  run remove --keep-branch "$WT"
  expect_rc 0
  git -C "$T/project" show-ref --verify --quiet refs/heads/half || fail "--keep-branch deleted the branch"

  make_worktree pushed
  echo change >>"$WT/README"
  git -C "$WT" commit -q -am "pushed, not merged locally"
  git -C "$WT" push -q -u origin pushed 2>/dev/null
  run remove "$WT"
  expect_rc 0
  [[ ! -e $WT ]] || fail "a branch merged into its upstream was refused"

  make_worktree discarded
  echo change >>"$WT/README"
  git -C "$WT" commit -q -am "to discard"
  run remove --force "$WT"
  expect_rc 0
  git -C "$T/project" show-ref --verify --quiet refs/heads/discarded && fail "--force kept the branch"
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

  # Past the unmerged check, so that only the order protects the remote: a lock on the
  # ref makes the local deletion itself fail.
  make_worktree locked-ref
  git -C "$WT" push -q -u origin locked-ref 2>/dev/null
  touch "$T/project/.git/refs/heads/locked-ref.lock"
  run remove --delete-remote "$WT"
  rm "$T/project/.git/refs/heads/locked-ref.lock"
  expect_rc 1
  has_remote_branch locked-ref || fail "the remote branch went before the local deletion failed"

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

# --- questions ----------------------------------------------------------------

# Everything a question could touch, by path, mtime, size and mode.
tree_state() {
  find "$@" -printf '%p %T@ %s %m\n' 2>/dev/null | sort
}

# check changes nothing on disk, even where guard would clear metadata. The file is
# touched first, so that a plain `git status` would rewrite the index.
t_check_changes_nothing() {
  make_project
  make_worktree asked
  touch "$WT/README" "$WT/sub/README"
  local before after
  before=$(tree_state "$WT" "$WTGIT")
  run check "$WT"
  expect_rc 0
  expect_out_mention "may be removed"
  after=$(tree_state "$WT" "$WTGIT")
  [[ $before == "$after" ]] || fail "check changed the disk:"$'\n'"$(diff <(echo "$before") <(echo "$after") | head -20)"
  run guard "$WT"
  expect_rc 0
  [[ ! -d $WTGIT/modules ]] || fail "setup: guard should have cleared what check left"
}

# status changes nothing either, and that includes a stranded worktree it has to verify.
t_status_changes_nothing() {
  make_project
  make_worktree looked
  touch "$WT/README" "$WT/sub/README"
  local before after
  before=$(tree_state "$WT" "$WTGIT")
  run status "$WT"
  expect_rc 0
  after=$(tree_state "$WT" "$WTGIT")
  [[ $before == "$after" ]] || fail "status changed the disk:"$'\n'"$(diff <(echo "$before") <(echo "$after") | head -20)"
}

# check and guard give the same answer in every state, because they are the same code.
t_check_agrees_with_guard() {
  make_project
  local state check_rc guard_rc
  for state in clean dirty dirty_sub unpushed main; do
    make_worktree "agree-$state"
    case $state in
    dirty) echo x >"$WT/scratch" ;;
    dirty_sub) echo x >>"$WT/sub/README" ;;
    unpushed)
      git -C "$WT/sub" checkout -q -b work
      echo x >>"$WT/sub/README"
      git -C "$WT/sub" commit -q -am unpushed
      ;;
    main) WT=$T/project ;;
    esac
    run check "$WT"
    check_rc=$rc
    run guard "$WT"
    guard_rc=$rc
    [[ $check_rc -eq $guard_rc ]] || fail "$state: check exited $check_rc, guard exited $guard_rc"
    [[ $state == clean || $check_rc -ne 0 ]] || fail "$state: check said removable"
  done
  make_worktree agree-garbage
  git -C "$WT" submodule -q deinit -f --all 2>/dev/null
  echo garbage >"$WTGIT/modules/sub/HEAD"
  run check "$WT"
  expect_rc 3
  expect_err "cannot inspect"
}

# A process inside the worktree is named and refuses removal, under --force too. The
# caller's own process and its ancestors do not count. --ignore-in-use lets through.
t_in_use_refuses_and_names_the_process() {
  make_project
  make_worktree busy
  (cd "$WT" && exec sleep 300) &
  local holder=$!
  local sub
  for sub in check guard; do
    run "$sub" "$WT"
    expect_rc 1
    expect_err "in use by sleep (pid $holder)"
    run "$sub" --force "$WT"
    expect_rc 1
    expect_err "in use by sleep"
  done
  run remove --force "$WT"
  expect_rc 1
  expect_err "in use by sleep"
  [[ -e $WT/sub/README ]] || fail "a refused removal emptied the submodule"
  run status "$WT"
  expect_rc 0
  [[ $out == *"refused"* && $out == *"in_use"* ]] || fail "status did not report the process: $out"
  run check --ignore-in-use "$WT"
  expect_rc 0
  kill "$holder"
  wait "$holder" 2>/dev/null

  # The caller's own shell, sitting in the worktree, is not a holder.
  out=$(cd "$WT" && bash -c '"$WORKTREE" check "$PWD"' 2>"$SCRATCH/stderr") && rc=0 || rc=$?
  err=$(cat "$SCRATCH/stderr")
  expect_rc 0
  run remove "$WT"
  expect_rc 0
}

# A locked worktree is refused naming the reason, under --force too, until unlocked.
t_lock_refuses_until_unlocked() {
  make_project
  make_worktree held
  run lock --reason "agent-7 is working here" "$WT"
  expect_rc 0
  [[ -f $WTGIT/locked ]] || fail "lock did not use git worktree lock"
  local sub
  for sub in check guard; do
    run "$sub" --force "$WT"
    expect_rc 1
    expect_err "agent-7 is working here"
  done
  run remove --force "$WT"
  expect_rc 1
  expect_err "agent-7 is working here"
  [[ -e $WT/sub/README ]] || fail "a refused removal emptied the submodule"
  run lock "$WT"
  expect_rc 1
  run unlock "$WT"
  expect_rc 0
  [[ ! -f $WTGIT/locked ]] || fail "unlock did not release it"
  run remove "$WT"
  expect_rc 0
  run unlock "$T/project"
  expect_rc 1
  expect_err "main worktree"
}

# Every worktree under the root gets a verdict; an orphan is unknown, never safe, and
# makes the exit status 3. Nothing is deleted.
t_status_lists_every_worktree() {
  make_project
  local root=$T/root safe busy created
  safe=$(cd "$T/project" && WORKTREE_ROOT=$root "$WORKTREE" create safe-one 2>/dev/null)
  busy=$(cd "$T/project" && WORKTREE_ROOT=$root "$WORKTREE" create dirty-one 2>/dev/null)
  echo x >"$busy/scratch"
  out=$(WORKTREE_ROOT=$root "$WORKTREE" status 2>"$SCRATCH/stderr") && rc=0 || rc=$?
  err=$(cat "$SCRATCH/stderr")
  expect_rc 0
  [[ $out == *"removable"*"$safe"* ]] || fail "the clean worktree is not removable: $out"
  [[ $out == *"refused"*"$busy"*"dirty"* ]] || fail "the dirty worktree is not refused: $out"

  mkdir -p "$root/project/orphan"
  echo "gitdir: $T/nowhere" >"$root/project/orphan/.git"
  out=$(WORKTREE_ROOT=$root "$WORKTREE" status 2>"$SCRATCH/stderr") && rc=0 || rc=$?
  err=$(cat "$SCRATCH/stderr")
  expect_rc 3
  [[ $out == *"unknown"*"$root/project/orphan"* ]] || fail "an orphan is not unknown: $out"
  [[ -f $busy/scratch && -d $root/project/orphan ]] || fail "status deleted something"

  out=$(env -u WORKTREE_ROOT "$WORKTREE" status 2>"$SCRATCH/stderr") && rc=0 || rc=$?
  err=$(cat "$SCRATCH/stderr")
  expect_rc 1
  expect_err "no placement root"
}

# --- seed files ---------------------------------------------------------------

# A project whose main branch declares .env and config/*.local, with both on disk, ignored,
# beside an ignored file nobody chose and a tracked file that matches the pattern.
make_seeded_project() {
  make_project
  local main=$T/project
  printf '.env\nconfig/*.local\n' >"$main/.gitignore"
  printf '.env\nconfig/*.local\ntracked.env\ndraft.txt\n' >"$main/.worktreeinclude"
  echo tracked >"$main/tracked.env"
  git -C "$main" add .gitignore .worktreeinclude tracked.env
  git -C "$main" commit -q -m "declare seed files"
  mkdir -p "$main/config"
  echo secret >"$main/.env"
  chmod 600 "$main/.env"
  echo local >"$main/config/app.local"
  echo chosen-by-nobody >"$main/other.log"
  echo listed-but-not-ignored >"$main/draft.txt"
  printf 'other.log\n' >>"$main/.git/info/exclude"
}

t_seed_copies_only_what_is_declared_and_ignored() {
  make_seeded_project
  git -C "$T/project" worktree add -q -b seeded "$T/wt-seeded" 2>/dev/null
  run provision "$T/wt-seeded"
  expect_rc 0
  expect_out_mention "2 seed file"
  [[ $(cat "$T/wt-seeded/.env") == secret ]] || fail ".env not copied"
  [[ $(stat -c %a "$T/wt-seeded/.env") == 600 ]] || fail "a private file became readable: $(stat -c %a "$T/wt-seeded/.env")"
  [[ $(cat "$T/wt-seeded/config/app.local") == local ]] || fail "a nested file was not copied"
  [[ ! -e $T/wt-seeded/other.log ]] || fail "copied an ignored file nobody declared"
  [[ ! -e $T/wt-seeded/draft.txt ]] || fail "copied a file git does not ignore"
  [[ -z $(git -C "$T/wt-seeded" status --porcelain) ]] || fail "seeding dirtied the worktree"
}

t_seed_never_overwrites() {
  make_seeded_project
  git -C "$T/project" worktree add -q -b seeded "$T/wt-seeded" 2>/dev/null
  echo mine >"$T/wt-seeded/.env"
  run provision "$T/wt-seeded"
  expect_rc 0
  [[ $(cat "$T/wt-seeded/.env") == mine ]] || fail "seeding overwrote an existing file"
  [[ $(cat "$T/wt-seeded/config/app.local") == local ]] || fail "one existing file stopped the rest"
}

# The list comes from the main checkout: a branch cannot widen what is copied into it.
t_seed_ignores_a_list_the_branch_changed() {
  make_seeded_project
  git -C "$T/project" worktree add -q -b seeded "$T/wt-seeded" 2>/dev/null
  printf 'other.log\n' >"$T/wt-seeded/.worktreeinclude"
  git -C "$T/wt-seeded" commit -q -am "ask for more"
  out=$(cd "$T/wt-seeded" && "$WORKTREE" provision . 2>"$SCRATCH/stderr") && rc=0 || rc=$?
  err=$(cat "$SCRATCH/stderr")
  expect_rc 0
  [[ ! -e $T/wt-seeded/other.log ]] || fail "a branch's own list was obeyed"
  [[ $(cat "$T/wt-seeded/.env") == secret ]] || fail "the main checkout's list was not obeyed"
}

# A branch can commit `config` as a symlink. Following it would write outside the worktree.
t_seed_does_not_write_through_a_symlink() {
  make_seeded_project
  mkdir "$T/outside"
  git -C "$T/project" worktree add -q --no-checkout -b seeded "$T/wt-seeded" 2>/dev/null
  git -C "$T/wt-seeded" checkout -q
  ln -s "$T/outside" "$T/wt-seeded/config"
  run provision "$T/wt-seeded"
  expect_rc 0
  expect_out_mention "config"
  [[ ! -e $T/outside/app.local ]] || fail "seeding wrote through a symlink"
  [[ $(cat "$T/wt-seeded/.env") == secret ]] || fail "one refused file stopped the rest"
}

# Only regular files: a symlink in the main checkout could point at anything.
t_seed_skips_a_symlink_source() {
  make_seeded_project
  rm "$T/project/.env"
  ln -s /etc/hostname "$T/project/.env"
  git -C "$T/project" worktree add -q -b seeded "$T/wt-seeded" 2>/dev/null
  run provision "$T/wt-seeded"
  expect_rc 0
  [[ ! -e $T/wt-seeded/.env && ! -L $T/wt-seeded/.env ]] || fail "a symlink source was copied"
}

t_seed_failure_warns_and_exits_zero() {
  make_seeded_project
  git -C "$T/project" worktree add -q -b seeded "$T/wt-seeded" 2>/dev/null
  chmod 000 "$T/project/.env"
  run provision "$T/wt-seeded"
  chmod 600 "$T/project/.env"
  expect_rc 0
  expect_out_mention ".env"
  [[ ! -e $T/wt-seeded/.env ]] || fail "left a partial copy behind"
  [[ $(cat "$T/wt-seeded/config/app.local") == local ]] || fail "one failure stopped the rest"

  # The whole step failing: git cannot list the files.
  git -C "$T/project" worktree add -q -b seeded-b "$T/wt-seeded-b" 2>/dev/null
  local fake=$T/fake-bin real_git
  real_git=$(command -v git)
  mkdir -p "$fake"
  printf '#!/usr/bin/env bash\ncase " $* " in *" ls-files "*) exit 128 ;; esac\nexec "%s" "$@"\n' "$real_git" >"$fake/git"
  chmod +x "$fake/git"
  out=$(PATH=$fake:$PATH "$WORKTREE" provision "$T/wt-seeded-b" 2>"$SCRATCH/stderr") && rc=0 || rc=$?
  err=$(cat "$SCRATCH/stderr")
  expect_rc 0
  expect_out_mention "seed step FAILED"
}

# Provisioning the main checkout has nothing to copy from, and says nothing.
t_seed_skips_the_main_checkout() {
  make_seeded_project
  run provision "$T/project"
  expect_rc 0
  [[ $out != *"seed"* && $err != *"seed"* ]] || fail "seeded the main checkout: $out $err"
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

# workmux's post_create is fail-fast, so a failed `direnv allow` warns and exits 0,
# and create still hands back the path. A path that is not a directory stays exit 1.
t_provision_never_fails_its_caller() {
  make_project
  local fake=$SCRATCH/$current/bin
  mkdir -p "$fake"
  printf '#!/usr/bin/env bash\n[[ $1 == status ]] && { echo "Found RC allowed 0"; exit 0; }\necho "allow broke" >&2; exit 7\n' >"$fake/direnv"
  chmod +x "$fake/direnv"
  echo 'export FOO=1' >"$T/project/.envrc"
  git -C "$T/project" add .envrc
  git -C "$T/project" commit -q -m envrc
  git -C "$T/project" worktree add -q -b failing "$T/wt-failing" 2>/dev/null
  out=$(PATH=$fake:$PATH "$WORKTREE" provision "$T/wt-failing" 2>"$SCRATCH/stderr") && rc=0 || rc=$?
  err=$(cat "$SCRATCH/stderr")
  expect_rc 0
  [[ $out == *"FAILED"* ]] || fail "a failed direnv allow did not warn: $out"
  [[ $out != *"worktree .envrc allowed"* ]] || fail "a failed direnv allow claimed trust: $out"
  out=$(cd "$T/project" && PATH=$fake:$PATH WORKTREE_ROOT=$T/root "$WORKTREE" create failing-create 2>"$SCRATCH/stderr") && rc=0 || rc=$?
  err=$(cat "$SCRATCH/stderr")
  expect_rc 0
  [[ $out == "$T/root/project/failing-create" ]] || fail "create did not hand back the path: $out"
  run provision "$T/no-such-directory"
  expect_rc 1
  expect_err "is not a directory"
}

# feat/x and feat-x share a slug. Asking for the second must refuse, naming both,
# rather than hand back a worktree on the first. Asking again for the first still
# returns its path; a directory that is no worktree at all is refused too.
t_create_refuses_a_slug_collision() {
  make_project
  local first=$T/root/project/feat-x created
  created=$(cd "$T/project" && WORKTREE_ROOT=$T/root "$WORKTREE" create feat/x 2>/dev/null)
  [[ $created == "$first" ]] || fail "setup: placed at '$created'"
  out=$(cd "$T/project" && WORKTREE_ROOT=$T/root "$WORKTREE" create feat-x 2>"$SCRATCH/stderr") && rc=0 || rc=$?
  err=$(cat "$SCRATCH/stderr")
  expect_rc 1
  expect_err "feat/x"
  expect_err "feat-x"
  [[ -z $out ]] || fail "a refused create printed a path: $out"
  out=$(cd "$T/project" && WORKTREE_ROOT=$T/root "$WORKTREE" create feat/x 2>"$SCRATCH/stderr") && rc=0 || rc=$?
  err=$(cat "$SCRATCH/stderr")
  expect_rc 0
  [[ $out == "$first" ]] || fail "the same branch did not get its path back: $out"
  mkdir -p "$T/root-outside/project/not-git"
  out=$(cd "$T/project" && WORKTREE_ROOT=$T/root-outside "$WORKTREE" create not/git 2>"$SCRATCH/stderr") && rc=0 || rc=$?
  err=$(cat "$SCRATCH/stderr")
  expect_rc 1
  expect_err "not a worktree"
  [[ -z $out ]] || fail "a refused create printed a path: $out"
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
  [[ $out == *"worktree .envrc allowed"* ]] || fail "a working direnv allow was not reported: $out"

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
  expect_out_mention "dry run"

  run sweep --apply "$root"
  expect_rc 0
  [[ $out != *"dry run"* ]] || fail "--apply called itself a dry run: $out"
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
  for sub in status check create provision actor guard remove lock unlock sweep; do
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
