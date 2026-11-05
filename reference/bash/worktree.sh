# FROZEN REFERENCE: the Bash implementation, copied from ai-harness at commit 2be0a66.
# Read it for behaviour and reasoning. Do not edit or ship it. Delete it after cutover.
#
# worktree — the git worktree lifecycle, one command, every harness.
#
#   worktree create <branch> [base]   add <root>/<repo>/<slug>, then provision it
#   worktree provision [path]         submodules, then gated direnv trust
#   worktree actor [dir]              the tracker actor for a directory
#   worktree guard [--force] [path]   may this worktree be removed? then clear
#                                     its stale submodule metadata; never removes
#   worktree remove [opts] <path>     guard, then remove, prune, delete branch
#   worktree sweep [--apply] [opts]   find leftovers; delete only with --apply
#
# Vocabulary: GLOSSARY.md at the root of the ai-harness repository. Built by
# modules/packages/_worktree.nix (writeShellApplication: errexit, nounset,
# pipefail, shellcheck at build time) and tested by ./test.sh — every guarantee
# below has a test there and a mutation in _worktree.nix that proves it can fail.
#
# EXIT STATUS — the contract every caller (workmux's pre_remove, the herdr
# wrapper, a human) relies on:
#   0  done / removable
#   1  refused, with the reason on stderr
#   2  usage
#   3  a probe could not run — the tool cannot TELL whether removal is safe, so
#      it stops and names the path. Never silent: the bug this replaces
#      (worktree-remove.sh, 2026-09-22) died with exit 128 and no output at all,
#      because its probe sent git's stderr to /dev/null under `set -e`.

WORKTREE_ROOT=${WORKTREE_ROOT:-}

usage() {
  cat >&2 <<'USAGE'
usage: worktree create <branch> [base]      add <root>/<repo>/<slug>, then provision it
       worktree provision [path]            submodules, then gated direnv trust
       worktree actor [dir]                 the tracker actor for a directory
       worktree guard [--force] [path]      may it be removed? clear stale metadata; never removes
       worktree remove [--force] [--keep-branch] [--delete-remote] <path>
       worktree sweep [--apply] [--repo <path>]... [<root>...]
USAGE
  exit 2
}

say() { echo "worktree: $*"; }

# For the subcommands that take positionals only (create, provision, actor): a
# leading dash is a request for help or a typo, never a branch or a path — git
# refuses branch names that start with one. Without this, `create --help` tried
# to make a branch called `--help` (observed 2026-10-06).
positional_only() {
  local arg
  for arg; do
    case $arg in
    -h | --help) usage ;;
    -*) refuse "$cmd: unknown option $arg" ;;
    esac
  done
}
refuse() {
  echo "worktree: $*" >&2
  exit 1
}

# Run git, and if it fails, stop LOUDLY with the command, the path it was about
# and git's own words. The one probe whose failure used to be silent is the one
# that decides whether a commit is about to be lost; a probe that cannot answer
# is not "nothing unpushed", it is "unknown", and unknown refuses.
probe() {
  local about=$1 out rc
  shift
  rc=0
  out=$(git "$@" 2>&1) || rc=$?
  if [[ $rc -ne 0 ]]; then
    {
      echo "worktree: cannot inspect $about (git exit $rc) — refusing, because an"
      echo "          unanswerable probe is not the same as a safe one."
      echo "          command: git $*"
      printf '          %s\n' "$out"
    } >&2
    exit 3
  fi
  printf '%s' "$out"
}

# Commits a git dir holds that no remote-tracking ref has: the one thing
# deleting a submodule clone can lose. `--all` rather than `--branches`, because
# a submodule is normally on a DETACHED head — a commit made there is on no
# branch at all — and because a stash is work too.
#
# `--work-tree=/` is the fix for the silent 128. After a deinit, a nested
# submodule's gitdir keeps `core.worktree` pointing at a directory that no
# longer exists, and git refuses to run in it at all — even for `log`, which
# never touches a work tree. Overriding the work tree with any directory that
# exists lets the history be read (measured 2026-09-23 by hand: all five gitdirs
# of a nested submodule chain then answered).
unpushed() {
  probe "$1" --git-dir="$1" --work-tree=/ log --oneline --all --not --remotes
}

# Every git dir under <worktree gitdir>/modules — populated submodules and
# deinitialised leftovers alike, nested ones included. A git dir is identified
# by its `objects` directory, which `logs/` and `refs/` never have.
module_gitdirs() {
  local modules=$1/modules head
  [[ -d $modules ]] || return 0
  while IFS= read -r head; do
    [[ -d $(dirname "$head")/objects ]] && dirname "$head"
  done < <(find "$modules" -name HEAD -type f | sort)
}

# A populated submodule whose `.git` file points at a gitdir that no longer
# exists: its clone is gone and git cannot run in it at all. Prints the missing
# gitdir, or nothing. An old guard made exactly this shape — it cleared
# <gitdir>/modules without deinitialising submodules in subdirectories
# — so it has to be recognised rather than just refused.
stranded_target() {
  local dir=$1 ptr
  [[ -f $dir/.git ]] || return 0
  ptr=$(sed -n 's/^gitdir: //p' "$dir/.git")
  [[ -n $ptr ]] || return 0
  [[ $ptr == /* ]] || ptr=$dir/$ptr
  [[ -e $ptr ]] || printf '%s\n' "$ptr"
}

# Populated submodules under a directory, recursively, found from the GITLINKS
# git records (mode 160000) — never from `git submodule status`, which reports
# a checked-out submodule as uninitialised when .git/config lacks its
# url/active keys (measured 2026-09-22). Printed as paths relative to $1.
#
# Never inside a process substitution: a probe that exits 3 there is swallowed
# and the list just comes back SHORT — the silent failure this tool exists to
# prevent. Call it with its output redirected to a file, in the current shell.
# A stranded submodule is listed but not descended into; git cannot read it.
populated_submodules() {
  local top=$1 prefix=${2:-} listing path
  listing=$(probe "$top/$prefix" -C "$top/$prefix" ls-files -s)
  while IFS= read -r path; do
    [[ -n $path && -e "$top/$prefix$path/.git" ]] || continue
    printf '%s\n' "$prefix$path"
    [[ -n $(stranded_target "$top/$prefix$path") ]] ||
      populated_submodules "$top" "$prefix$path/"
  done <<<"$(awk '$1 == "160000" { print $4 }' <<<"$listing")"
}

# Is $1 inside another path of the list that follows? Top-level means "not
# inside another submodule" — NOT "no slash in its path": `lib/shared` is as
# top-level as `sub`.
nested_in() {
  local sub=$1 other
  shift
  for other in "$@"; do
    [[ $sub == "$other"/* ]] && return 0
  done
  return 1
}

# A submodule's name at a commit, from that commit's .gitmodules. The name, not
# the path, is what its clone lives under in modules/.
submodule_name() {
  local gitdir=$1 commit=$2 path=$3
  probe "$gitdir" --git-dir="$gitdir" --work-tree=/ config --blob "$commit:.gitmodules" --get-regexp '^submodule\..*\.path$' |
    awk -v p="$path" '$2 == p { sub(/^submodule\./, "", $1); sub(/\.path$/, "", $1); print $1; exit }'
}

# The first of the candidate clones that holds the commit, or nothing. Any
# clone of the submodule will do: a commit is the same object in all of them.
# Addressed by --git-dir, never through the submodule's gitfile, whose relative
# path goes stale when the repository moves.
clone_with() {
  local commit=$1 clone
  shift
  for clone in "$@"; do
    [[ -d $clone ]] || continue
    git --git-dir="$clone" --work-tree=/ cat-file -e "$commit^{commit}" 2>/dev/null || continue
    printf '%s\n' "$clone"
    return 0
  done
}

# Prove a stranded submodule's files hold nothing its recorded commit does not:
# compare the directory against that commit, read from another clone of the
# same submodule — the first of the candidates that holds it — through a
# throwaway index. Ignored files go, as `submodule deinit -f` would take them;
# a modified or untracked file refuses (1). Nested submodules are checked the
# same way, recursively; a nested one no clone can answer for stops (3).
verify_stranded() {
  local dir=$1 clone=$2 commit=$3 label=$4 idx diff tree npath ncommit nname nclone
  idx=$(mktemp)
  GIT_INDEX_FILE=$idx probe "$label" --git-dir="$clone" --work-tree=/ read-tree "$commit" >/dev/null
  diff=$(GIT_INDEX_FILE=$idx probe "$label" --git-dir="$clone" --work-tree="$dir" status --porcelain --untracked-files=all --ignore-submodules=all)
  rm -f "$idx"
  [[ -z $diff ]] || refuse "stranded submodule $label differs from its recorded commit ${commit:0:12} — keep what you need from it first:"$'\n'"$diff"
  # Assigned, not piped into the loop: a redirection's substitution status is
  # never checked, so a failed probe there would read as "no nested submodules".
  # --full-tree: with --work-tree=/ git takes the CALLER's directory as a path
  # prefix, and a bare `ls-tree -r` then lists nothing at all — which is how a
  # modified nested submodule was once emptied unchecked (caught by
  # t_stranded_modified_refused before it shipped).
  tree=$(probe "$label" --git-dir="$clone" --work-tree=/ ls-tree -r --full-tree "$commit")
  while read -r npath ncommit; do
    [[ -n $npath && -e $dir/$npath/.git ]] || continue
    nname=$(submodule_name "$clone" "$commit" "$npath")
    [[ -n $nname ]] || refuse "stranded submodule $label/$npath is not in $label's .gitmodules at ${commit:0:12}"
    nclone=$(clone_with "$ncommit" "$clone/modules/$nname")
    if [[ -z $nclone ]]; then
      {
        echo "worktree: cannot verify stranded submodule $label/$npath: no clone holds its recorded commit ${ncommit:0:12}. Looked in:"
        echo "          $clone/modules/$nname"
        echo "          Clone it where the guard looks, then retry:"
        clone_advice "$clone" "$commit" "$nname" "$(git --git-dir="$clone" config --get remote.origin.url 2>/dev/null)" "$clone/modules/$nname"
      } >&2
      exit 3
    fi
    verify_stranded "$dir/$npath" "$nclone" "$ncommit" "$label/$npath"
  done <<<"$(awk '$2 == "commit" { print $4, $3 }' <<<"$tree")"
}

# The command that puts a clone of submodule <name> where the guard looks: its
# url read from <gitdir>'s .gitmodules at <commit>. A relative url is resolved
# against <base>, the url of the repository that declares it, as git resolves
# it: each ../ drops one component.
clone_advice() {
  local gitdir=$1 commit=$2 name=$3 base=$4 target=$5 url
  url=$(git --git-dir="$gitdir" --work-tree=/ config --blob "$commit:.gitmodules" --get "submodule.$name.url" 2>/dev/null) || url="<its url>"
  if [[ ($url == ./* || $url == ../*) && -n $base ]]; then
    base=${base%/}
    while [[ $url == ./* || $url == ../* ]]; do
      case $url in
      ./*) url=${url#./} ;;
      ../*) url=${url#../} base=${base%/*} ;;
      esac
    done
    url=$base/$url
  fi
  echo "          git clone --bare $url $target"
}

# No clone anywhere can answer for a stranded submodule: say how to get one,
# in a way that works whether or not the main checkout still declares it.
# Stops (3) — never --force past it: its files may exist nowhere else.
no_clone_for() {
  local sub=$1 name=$2 commit=$3 head=$4
  shift 4
  {
    echo "worktree: cannot verify stranded submodule $sub: no clone holds its recorded commit ${commit:0:12}. Looked in:"
    printf '          %s\n' "$@"
    if git -C "$main_wt" config --file .gitmodules --get "submodule.$name.path" >/dev/null 2>&1; then
      echo "          Initialise the submodule in the main checkout (git -C $main_wt submodule update --init --recursive), then retry."
    else
      echo "          The main checkout no longer declares it. Clone it where the guard looks, then retry:"
      clone_advice "$common" "$head" "$name" "$(git -C "$main_wt" remote get-url origin 2>/dev/null)" "$common/modules/$name"
    fi
  } >&2
  exit 3
}

# Resolve a path to a LINKED worktree: sets wt, gitdir, common, main_wt.
resolve_worktree() {
  local path=$1
  [[ -d $path ]] || refuse "$path is not a directory"
  wt=$(cd "$path" && pwd -P)
  wt=$(probe "$wt" -C "$wt" rev-parse --show-toplevel)
  gitdir=$(probe "$wt" -C "$wt" rev-parse --absolute-git-dir)
  common=$(probe "$wt" -C "$wt" rev-parse --path-format=absolute --git-common-dir)
  [[ $gitdir != "$common" ]] || refuse "$wt is the main worktree, not a linked one"
  main_wt=$(dirname "$common")
}

# --- actor ------------------------------------------------------------------

# The tracker actor for a directory: the name of the linked worktree it is in,
# or nothing. One worktree hosts one agent at a time, so the worktree IS the
# actor — for every harness at once, which is why it is derived here instead of
# written into each harness's own settings file (only Claude has one that needs
# no trust; agy has none at all — measured 2026-09-23).
#
# Deliberately quiet: outside git, or in a main checkout, there is no actor and
# bd falls back to its own default. Nothing here decides whether data is safe.
cmd_actor() {
  positional_only "$@"
  local dir=${1:-$PWD} g c top
  [[ -d $dir ]] || return 0
  g=$(git -C "$dir" rev-parse --absolute-git-dir 2>/dev/null) || return 0
  c=$(git -C "$dir" rev-parse --path-format=absolute --git-common-dir 2>/dev/null) || return 0
  [[ $g != "$c" ]] || return 0
  top=$(git -C "$dir" rev-parse --show-toplevel 2>/dev/null) || return 0
  basename "$top"
}

# --- provision --------------------------------------------------------------

# 1. Submodules. `git worktree add` leaves submodules UNINITIALISED — observed
#    2026-08-24, where the first `nix develop` in a fresh worktree failed on an
#    empty submodule until `git submodule update --init` ran by hand. The
#    machine-global submodule.recurse=true does not cover worktree creation.
#
#    Never fails the caller: workmux's post_create is FAIL-FAST (a non-zero
#    hook aborts every remaining one and leaves the worktree half-created,
#    measured on 0.1.240), so an unreachable submodule remote — offline, the
#    forge down — must not cost the worktree its direnv trust below.
provision_submodules() {
  [[ -f .gitmodules ]] || return 0
  if git submodule update --init --recursive; then
    say "submodules initialised"
  else
    say "submodule init FAILED (offline?) — run 'git submodule update --init --recursive' by hand before 'nix develop'"
  fi
}

# 2. Inherit direnv trust into a fresh worktree — only when provably safe.
#    direnv's allow DB is keyed by path+content (verified 2026-08-24 against
#    direnv 2.37.1: an identical .envrc copied to a new path reports
#    `Found RC allowed 1`), so a worktree is never trusted merely by existing.
#    This allows the worktree's .envrc IFF the MAIN checkout's .envrc is itself
#    currently allowed AND the worktree copy is byte-identical to it — exactly
#    the bytes the user already reviewed. A worktree made from another base can
#    carry a DIFFERENT .envrc; that one gets a message and a manual review, not
#    inherited trust. Deliberately NOT a direnv.toml whitelist.prefix on the
#    placement root: a prefix whitelist trusts every FUTURE .envrc under the
#    path, content unseen, and never re-prompts.
provision_direnv() {
  local root
  [[ -f .envrc ]] || return 0
  command -v direnv >/dev/null 2>&1 || return 0
  root=$(dirname "$(git rev-parse --path-format=absolute --git-common-dir)")
  [[ -f $root/.envrc ]] || return 0

  # Gate 1 — the root's .envrc is allowed. direnv 2.37 prints the numeric enum
  # (0 = allowed); older versions printed `true`.
  (cd "$root" && direnv status 2>/dev/null | grep -Eq '^Found RC allowed (0|true)$') || return 0

  # Gate 2 — the same bytes the user already reviewed.
  if ! cmp -s .envrc "$root/.envrc"; then
    say "direnv: this worktree's .envrc differs from the allowed root copy — review it and run 'direnv allow' yourself"
    return 0
  fi

  direnv allow .
  say "direnv: worktree .envrc allowed (byte-identical to the allowed root .envrc)"
}

# In the order they must run: a populated tree before anything reads it, trust
# last so a broken tree is never trusted. There is no actor step: the actor is
# derived when bd runs (cmd_actor), not written at creation.
cmd_provision() {
  positional_only "$@"
  local path=${1:-.}
  cd "$path" || refuse "$path is not a directory"
  provision_submodules
  provision_direnv
}

# --- create -----------------------------------------------------------------

slug_of() { printf '%s' "$1" | tr '/ ' '--'; }

cmd_create() {
  positional_only "$@"
  local branch=${1:-} base=${2:-HEAD} root repo path
  [[ -n $branch ]] || usage
  [[ -n $WORKTREE_ROOT ]] || refuse "no placement root: set WORKTREE_ROOT (aiHarness.worktree.root sets it for the installed command)"
  root=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null | xargs -r dirname) || true
  [[ -n $root ]] || refuse "not inside a git repository"
  repo=$(basename "$root")
  path=$WORKTREE_ROOT/$repo/$(slug_of "$branch")

  if [[ -e $path ]]; then
    say "$path exists — not creating it again" >&2
  else
    mkdir -p "$(dirname "$path")"
    if git -C "$root" show-ref --verify --quiet "refs/heads/$branch"; then
      git -C "$root" worktree add "$path" "$branch" >&2
    else
      git -C "$root" worktree add -b "$branch" "$path" "$base" >&2
    fi
    (cmd_provision "$path") >&2
  fi
  printf '%s\n' "$path"
}

# --- guard ------------------------------------------------------------------

# May this worktree be removed? Removal destroys two things nothing else keeps:
# the worktree's uncommitted changes, and its SUBMODULE CLONES — each worktree
# gets its own under <gitdir>/modules, and a commit made in one exists nowhere
# else until pushed. The worktree's own commits are not at risk here: they live
# in the shared repository, and whether its branch is deleted is the removing
# tool's decision (workmux asks; `worktree remove` uses `branch -d`).
#
# Then clears <gitdir>/modules: git refuses `worktree remove` on that
# directory's mere EXISTENCE (builtin/worktree.c validate_no_submodules), and
# nothing else ever deletes it. That refusal is reached only WITHOUT --force —
# `git worktree remove --force` goes straight past it (git source, and measured
# 2026-09-23: empty modules/ → plain rc=128, --force rc=0). So the value of this
# guard is the unpushed check, not getting past git. Never "simplify" it into
# `--force`: that deletes submodule clones unchecked.
#
# --force: accept losing the worktree's OWN uncommitted changes. Submodule
# changes and unpushed submodule commits are refused regardless.
cmd_guard() {
  local force=0 path="" sub dirty d pending
  while [[ $# -gt 0 ]]; do
    case $1 in
    --force) force=1 ;;
    -h | --help) usage ;;
    -*) refuse "guard: unknown option $1" ;;
    *)
      [[ -z $path ]] || usage
      path=$1
      ;;
    esac
    shift
  done
  # workmux runs pre_remove hooks in the worktree and names it in the env.
  path=${path:-${WM_WORKTREE_PATH:-$PWD}}
  resolve_worktree "$path"

  if [[ $force -eq 0 ]]; then
    dirty=$(probe "$wt" -C "$wt" status --porcelain --ignore-submodules=all)
    [[ -z $dirty ]] || refuse "$wt has uncommitted changes — commit them, or discard them with 'worktree remove --force $wt':"$'\n'"$dirty"
  fi

  local subs=() live=() stranded=() list
  list=$(mktemp)
  populated_submodules "$wt" >"$list"
  mapfile -t subs <"$list"
  for sub in "${subs[@]}"; do
    if [[ -n $(stranded_target "$wt/$sub") ]]; then
      stranded+=("$sub")
    else
      live+=("$sub")
    fi
  done
  for sub in "${live[@]}"; do
    dirty=$(probe "$wt/$sub" -C "$wt/$sub" status --porcelain --ignore-submodules=all)
    [[ -z $dirty ]] || refuse "submodule $sub in $wt has uncommitted changes — commit and push them there first:"$'\n'"$dirty"
  done

  # A stranded submodule's gitfile leads nowhere. Its recorded commit, read
  # from another clone of the same submodule, is what its files are compared
  # against. Only a top-level one can be traced to such a clone.
  local name commit head clone
  [[ ${#stranded[@]} -eq 0 ]] || head=$(probe "$wt" -C "$wt" rev-parse HEAD)
  for sub in "${stranded[@]}"; do
    nested_in "$sub" "${subs[@]}" &&
      refuse "stranded submodule $sub sits inside a live one; cannot trace its clone — inspect it by hand"
    name=$(submodule_name "$common" "$head" "$sub")
    [[ -n $name ]] || refuse "stranded submodule $sub is not in $wt's .gitmodules"
    commit=$(probe "$wt" -C "$wt" ls-files -s -- "$sub" | awk '{ print $2; exit }')
    # The main checkout's clone first; then the worktree's own, which is the
    # only one left once main has RETIRED the submodule.
    clone=$(clone_with "$commit" "$common/modules/$name" "$gitdir/modules/$name")
    [[ -n $clone ]] || no_clone_for "$sub" "$name" "$commit" "$head" "$common/modules/$name" "$gitdir/modules/$name"
    verify_stranded "$wt/$sub" "$clone" "$commit" "$sub"
  done

  while IFS= read -r d; do
    [[ -n $d ]] || continue
    pending=$(unpushed "$d")
    [[ -z $pending ]] || refuse "$d holds commits no remote has — a submodule clone that exists only in this worktree; push them first:"$'\n'"$pending"
  done < <(module_gitdirs "$gitdir")

  # Only now, with every clone proven disposable: empty and clear.
  # Top-level = not inside another submodule, whatever its depth on disk.
  local top=()
  for sub in "${live[@]}"; do
    nested_in "$sub" "${subs[@]}" || top+=("$sub")
  done
  # Emptied by hand — the working-tree half of `submodule deinit -f`, and only
  # that half. deinit also unsets submodule.<name>.url/active/update, and that
  # config is SHARED by every worktree: run in a linked worktree it de-registers
  # the submodules of the main checkout (measured 2026-09-27).
  # Every file here was just proven clean, every clone pushed.
  for sub in "${top[@]}"; do
    find "$wt/$sub" -mindepth 1 -delete
  done
  [[ ${#top[@]} -eq 0 ]] || say "emptied ${#top[@]} submodule(s) in $wt (all clean, all pushed)"
  # The same for a stranded one: git cannot run in it, and every file in it
  # was just proven to be in its recorded commit.
  for sub in "${stranded[@]}"; do
    find "$wt/$sub" -mindepth 1 -delete
    say "emptied stranded submodule $sub (its files match ${sub}'s recorded commit)"
  done

  # Never clear modules/ under a submodule that is still populated: that is
  # the shape nothing can inspect or remove afterwards.
  populated_submodules "$wt" >"$list"
  if [[ -s $list ]]; then
    refuse "still populated after emptying, so $gitdir/modules is NOT cleared (clearing it would strand them):"$'\n'"$(cat "$list")"
  fi
  rm -f "$list"
  if [[ -d $gitdir/modules ]]; then
    rm -rf "$gitdir/modules"
    say "cleared $gitdir/modules (submodule clones, all pushed; git refuses removal while it exists)"
  fi
  say "$wt may be removed"
}

# --- remove -----------------------------------------------------------------

# Guard, then remove. Order matters and is the old script's: the remote branch
# is deleted LAST, and only on request — a chain that deletes it first and then
# fails locally leaves the local branch and worktree alive with their upstream
# gone.
cmd_remove() {
  local keep_branch=0 delete_remote=0 force=0 path="" branch remote
  while [[ $# -gt 0 ]]; do
    case $1 in
    --keep-branch) keep_branch=1 ;;
    --delete-remote) delete_remote=1 ;;
    --force) force=1 ;;
    -h | --help) usage ;;
    -*) refuse "remove: unknown option $1" ;;
    *)
      [[ -z $path ]] || usage
      path=$1
      ;;
    esac
    shift
  done
  [[ -n $path ]] || usage

  local guard_args=()
  [[ $force -eq 1 ]] && guard_args+=(--force)
  (cmd_guard "${guard_args[@]}" "$path")
  resolve_worktree "$path"
  branch=$(probe "$wt" -C "$wt" branch --show-current)

  if [[ $PWD == "$wt" || $PWD == "$wt"/* ]]; then
    say "note: your shell's cwd is inside $wt — cd elsewhere afterwards" >&2
  fi
  [[ -z $branch ]] || probe "$wt" -C "$wt" checkout -q --detach >/dev/null

  local remove_args=()
  [[ $force -eq 1 ]] && remove_args+=(--force)
  probe "$wt" -C "$main_wt" worktree remove "${remove_args[@]}" "$wt" >/dev/null
  probe "$main_wt" -C "$main_wt" worktree prune >/dev/null
  say "removed $wt"

  if [[ -z $branch ]]; then
    say "the worktree was detached; no branch to delete"
    return 0
  fi
  if [[ $keep_branch -eq 1 ]]; then
    say "kept branch $branch"
    return 0
  fi
  if [[ $force -eq 1 ]]; then
    git -C "$main_wt" branch -D "$branch"
  else
    git -C "$main_wt" branch -d "$branch" ||
      refuse "branch $branch is not merged — the worktree is gone; delete the branch with 'git branch -D $branch' once you have decided"
  fi

  if [[ $delete_remote -eq 1 ]]; then
    remote=$(git -C "$main_wt" config --get "branch.$branch.remote" || echo origin)
    if git -C "$main_wt" ls-remote --exit-code --heads "$remote" "$branch" >/dev/null 2>&1; then
      git -C "$main_wt" push "$remote" --delete "$branch"
    else
      say "$remote has no branch $branch; nothing to delete there"
    fi
  fi
}

# --- sweep ------------------------------------------------------------------

# Leftovers of removed worktrees, found — and deleted only with --apply. No
# timer runs this: an unattended delete is a new data-loss path.
#
# Under each placement root, a directory (depth 1 or 2) is:
#   live      a registered worktree (its .git file points at a gitdir that
#             points back) — untouched, and so is everything above it;
#   orphan    has a .git file whose gitdir is gone — REPORTED, never deleted:
#             its files may be uncommitted work nothing else holds;
#   repo      a full clone (.git directory) — untouched;
#   leftover  anything else: ignored files a removal left behind (a dev
#             server's cache, say), or an empty shell. Deleted by --apply.
#
# And per repository (--repo, or every repository a live worktree under a root
# belongs to, or the one you are in): registrations whose worktree directory is
# gone. `git worktree prune` would delete their gitdirs — and with them any
# submodule clone inside — so their clones are checked for unpushed commits
# first, and a repository with one is not pruned.
#
# Git's pointers are relative under worktree.useRelativePaths, each to the
# directory holding it — never to the cwd — so they are only ever read through
# this. The result is canonical, so two of them can be compared as strings.
resolve_from() {
  local base=$1 path=$2
  [[ $path == /* ]] || path=$base/$path
  realpath -m -- "$path"
}

sweep_classify() {
  local d=$1 ptr
  if [[ -d $d/.git ]]; then
    echo repo
  elif [[ -f $d/.git ]]; then
    ptr=$(sed -n 's/^gitdir: //p' "$d/.git")
    [[ -z $ptr ]] || ptr=$(resolve_from "$d" "$ptr")
    if [[ -n $ptr && -f $ptr/gitdir && $(resolve_from "$d" .git) == "$(resolve_from "$ptr" "$(cat "$ptr/gitdir")")" ]]; then
      echo live
    else
      echo orphan
    fi
  else
    echo other
  fi
}

cmd_sweep() {
  local apply=0 roots=() repos=() r g c kind top
  while [[ $# -gt 0 ]]; do
    case $1 in
    --apply) apply=1 ;;
    --repo)
      [[ $# -ge 2 ]] || usage
      repos+=("$2")
      shift
      ;;
    -h | --help) usage ;;
    -*) refuse "sweep: unknown option $1" ;;
    *) roots+=("$1") ;;
    esac
    shift
  done
  if [[ ${#roots[@]} -eq 0 ]]; then
    [[ -n $WORKTREE_ROOT ]] || refuse "sweep: no placement root given and WORKTREE_ROOT is unset"
    roots=("$WORKTREE_ROOT")
  fi
  if top=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null); then
    repos+=("$(dirname "$top")")
  fi

  local leftovers=() orphans=()
  # One entry: a live worktree names its repository for the prune pass below.
  note() {
    case $1 in
    live) repos+=("$(dirname "$(git -C "$2" rev-parse --path-format=absolute --git-common-dir)")") ;;
    orphan) orphans+=("$2") ;;
    esac
  }
  for r in "${roots[@]}"; do
    [[ -d $r ]] || continue
    r=$(cd "$r" && pwd -P)
    # Depth 1 is a group (<root>/<repo>) or, in a flat layout, a worktree.
    while IFS= read -r g; do
      kind=$(sweep_classify "$g")
      if [[ $kind != other ]]; then
        note "$kind" "$g"
        continue
      fi
      local kept=0 others=()
      while IFS= read -r c; do
        kind=$(sweep_classify "$c")
        if [[ $kind == other ]]; then
          others+=("$c")
        else
          kept=1
          note "$kind" "$c"
        fi
      done < <(find "$g" -mindepth 1 -maxdepth 1 -type d | sort)
      # A group holding nothing kept is one leftover, reported once; otherwise
      # each of its non-worktree children is.
      if [[ $kept -eq 0 ]]; then
        leftovers+=("$g")
      else
        leftovers+=("${others[@]}")
      fi
    done < <(find "$r" -mindepth 1 -maxdepth 1 -type d | sort)
  done

  for c in "${orphans[@]}"; do
    say "orphan   $c — a .git file whose gitdir is gone; inspect by hand, never swept"
  done
  for c in "${leftovers[@]}"; do
    if [[ $apply -eq 1 ]]; then
      rm -rf "$c"
      say "deleted  $c"
    else
      say "leftover $c ($(du -sh "$c" 2>/dev/null | cut -f1))"
    fi
  done

  # Registrations whose directory is gone, each judged ON ITS OWN: `git
  # worktree prune` works on the whole repository, so one registration holding
  # an unpushed clone would otherwise keep every clean one alive. A clean one is
  # pruned by deleting its admin directory — exactly what prune does per entry.
  # Locked registrations (`git worktree lock`, e.g. a worktree on removable
  # media) are left alone, as prune leaves them.
  local repo id d wtdir pending blocked seen=" "
  for repo in "${repos[@]}"; do
    [[ -d $repo ]] || continue
    repo=$(cd "$repo" && pwd -P)
    [[ $seen == *" $repo "* ]] && continue
    seen+="$repo "
    local common
    common=$(probe "$repo" -C "$repo" rev-parse --path-format=absolute --git-common-dir)
    for id in "$common"/worktrees/*; do
      [[ -f $id/gitdir && ! -e $id/locked ]] || continue
      wtdir=$(dirname "$(resolve_from "$id" "$(cat "$id/gitdir")")")
      [[ ! -e $wtdir ]] || continue
      blocked=0
      while IFS= read -r d; do
        [[ -n $d ]] || continue
        pending=$(unpushed "$d")
        if [[ -n $pending ]]; then
          say "stale    $wtdir — its submodule clone $d holds commits no remote has; NOT pruning it"
          blocked=1
        fi
      done < <(module_gitdirs "$id")
      [[ $blocked -eq 0 ]] || continue
      if [[ $apply -eq 1 ]]; then
        rm -rf "$id"
        say "pruned   $wtdir (registration in $repo)"
      else
        say "stale    $wtdir (registered in $repo; its directory is gone)"
      fi
    done
  done

  [[ $apply -eq 1 ]] || say "dry run — nothing deleted; pass --apply to delete the leftovers listed"
}

# --- dispatch ---------------------------------------------------------------

[[ $# -gt 0 ]] || usage
cmd=$1
shift
case $cmd in
create) cmd_create "$@" ;;
provision) cmd_provision "$@" ;;
actor) cmd_actor "$@" ;;
guard) cmd_guard "$@" ;;
remove) cmd_remove "$@" ;;
sweep) cmd_sweep "$@" ;;
-h | --help | help) usage ;;
*)
  echo "worktree: unknown command '$cmd'" >&2
  usage
  ;;
esac
