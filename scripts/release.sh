#!/bin/sh
# Usage: scripts/release.sh <version>
# Bumps the workspace version through an issue and PR, publishes every crate to crates.io,
# then tags v<version>; the tag's workflow makes the GitHub release with the APK and Linux binaries.
# After a failure, fix the cause and run it again with the same version: finished steps are skipped.
set -eu
v=${1:?usage: scripts/release.sh <version>}
cd "$(git rev-parse --show-toplevel)"

i=0
step() { i=$((i + 1)); echo "[$i/8] $*..."; }
done_already() { echo "      already done"; }

n=$(gh issue list --state all --search "\"Release $v\" in:title" --json number,title \
  -q "[.[] | select(.title == \"Release $v\")][0].number // empty")
branch=
[ -z "$n" ] || branch="$n-release-$v"
pr_state=
[ -z "$branch" ] || pr_state=$(gh pr list --head "$branch" --state all --json state -q '.[0].state // empty')

if [ "$pr_state" != MERGED ]; then
  current=$(git branch --show-current)
  if [ "$current" = main ]; then
    git diff --quiet HEAD || { echo "commit or stash your changes first" >&2; exit 1; }
    git pull -q --ff-only
  elif [ -n "$branch" ] && [ "$current" = "$branch" ]; then
    # A failed build after the version bump leaves exactly these two files modified.
    git diff --quiet HEAD -- . ':!Cargo.toml' ':!Cargo.lock' || { echo "commit or stash your changes first" >&2; exit 1; }
  else
    echo "run on main or on the release branch" >&2
    exit 1
  fi

  step "Opening issue 'Release $v'"
  if [ -z "$n" ]; then
    n=$(gh issue create --title "Release $v" --label "Q3 quick" --body "Bump the workspace version to $v and publish it." | grep -o '[0-9]*$')
    branch="$n-release-$v"
  else
    done_already
  fi

  if [ "$(git branch --show-current)" != "$branch" ]; then
    if git show-ref -q --verify "refs/heads/$branch"; then
      git checkout -q "$branch"
    elif git ls-remote -q --exit-code --heads origin "$branch" >/dev/null; then
      git fetch -q origin "$branch"
      git checkout -q -b "$branch" "origin/$branch"
    else
      git checkout -q -b "$branch"
    fi
  fi

  step "Bumping version in Cargo.toml and Cargo.lock to $v"
  # The workspace version and the crates' requirements on each other, all in the root Cargo.toml.
  old=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml)
  if [ "$old" != "$v" ]; then
    sed -i "s/version = \"$old\"/version = \"$v\"/" Cargo.toml
  else
    done_already
  fi
  cargo build -p yeda

  step "Committing, pushing branch $branch and opening PR"
  if ! git diff --quiet HEAD; then
    git commit -qam "Release $v (#$n)"
  fi
  git push -q -u origin HEAD
  if [ -z "$(gh pr list --head "$branch" --state open --json number -q '.[0].number // empty')" ]; then
    gh pr create --title "Release $v" --body "Closes #$n"
  else
    done_already
  fi

  step "Waiting for CI on the PR"
  # CI registers its checks a while after the PR opens; until then there is nothing to watch.
  until [ "$(gh pr view "$branch" --json statusCheckRollup -q '.statusCheckRollup | length')" -gt 0 ]; do sleep 5; done
  gh pr checks "$branch" --watch --fail-fast

  step "Merging PR and switching to main"
  # GitHub deletes the remote branch on merge; gh's --delete-branch would stop the script if its local cleanup failed.
  gh pr merge "$branch" --squash
else
  i=4
  step "Merging PR and switching to main"
  done_already
fi
git checkout -q main
git pull -q --ff-only
if [ -n "$branch" ] && git show-ref -q --verify "refs/heads/$branch"; then
  git branch -q -D "$branch"
fi

step "Building"
cargo build -p yeda

step "Publishing crates to crates.io"
published() {
  case ${#1} in
    1) p=1 ;;
    2) p=2 ;;
    3) p=3/$(echo "$1" | cut -c1) ;;
    *) p=$(echo "$1" | cut -c1-2)/$(echo "$1" | cut -c3-4) ;;
  esac
  curl -fsS "https://index.crates.io/$p/$1" 2>/dev/null | grep -q "\"vers\":\"$v\""
}
set -- --exclude yeda-android
pending=0
for crate in $(cargo metadata --no-deps --format-version 1 | jq -r '.packages[] | select(.publish == null) | .name'); do
  if published "$crate"; then
    set -- "$@" --exclude "$crate"
  else
    pending=1
  fi
done
if [ "$pending" = 1 ]; then
  # --allow-dirty: the reader's dist/ and Tauri's schemas are build outputs git ignores.
  cargo publish --workspace "$@" --allow-dirty
else
  done_already
fi

step "Tagging and pushing v$v"
if ! git show-ref -q --verify "refs/tags/v$v"; then
  git tag "v$v"
fi
if git ls-remote -q --exit-code --tags origin "v$v" >/dev/null; then
  done_already
else
  git push -q origin "v$v"
fi
