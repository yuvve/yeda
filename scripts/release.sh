#!/bin/sh
# Usage: scripts/release.sh <version>
# Bumps the workspace version through an issue and PR, publishes every crate to crates.io,
# then tags v<version>; the tag's workflow makes the GitHub release with the APK and Linux binaries.
set -eu
v=${1:?usage: scripts/release.sh <version>}
cd "$(git rev-parse --show-toplevel)"
[ "$(git branch --show-current)" = main ] || { echo "run on main" >&2; exit 1; }
git diff --quiet HEAD || { echo "commit or stash your changes first" >&2; exit 1; }
git pull -q --ff-only

i=0
step() { i=$((i + 1)); echo "[$i/8] $*..."; }

step "Opening issue 'Release $v'"
n=$(gh issue create --title "Release $v" --label "Q3 quick" --body "Bump the workspace version to $v and publish it." | grep -o '[0-9]*$')
git checkout -q -b "$n-release-$v"
step "Bumping version in Cargo.toml and Cargo.lock to $v"
# The workspace version and the crates' requirements on each other, all in the root Cargo.toml.
old=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml)
sed -i "s/version = \"$old\"/version = \"$v\"/" Cargo.toml
cargo build -p yeda
step "Committing, pushing branch $n-release-$v and opening PR"
git commit -qam "Release $v (#$n)"
git push -q -u origin HEAD
gh pr create --title "Release $v" --body "Closes #$n"
step "Waiting for CI on the PR"
# CI registers its checks a while after the PR opens; until then there is nothing to watch.
until [ "$(gh pr view --json statusCheckRollup -q '.statusCheckRollup | length')" -gt 0 ]; do sleep 5; done
gh pr checks --watch --fail-fast
step "Merging PR and switching to main"
# GitHub deletes the remote branch on merge; gh's --delete-branch would stop the script if its local cleanup failed.
gh pr merge --squash
git checkout -q main
git pull -q --ff-only
git branch -q -D "$n-release-$v"

step "Building"
cargo build -p yeda
step "Publishing crates to crates.io"
# --allow-dirty: the reader's dist/ and Tauri's schemas are build outputs git ignores.
cargo publish --workspace --exclude yeda-android --allow-dirty
step "Tagging and pushing v$v"
git tag "v$v"
git push -q origin "v$v"
