#!/bin/sh
# Usage: scripts/release.sh <version>
# Bumps the workspace version through an issue and PR, publishes every crate to crates.io,
# then tags v<version> and makes its GitHub release.
set -eu
v=${1:?usage: scripts/release.sh <version>}
cd "$(git rev-parse --show-toplevel)"
[ "$(git branch --show-current)" = main ] || { echo "run on main" >&2; exit 1; }
git diff --quiet HEAD || { echo "commit or stash your changes first" >&2; exit 1; }
git pull -q --ff-only

n=$(gh issue create --title "Release $v" --label "Q3 quick" --body "Bump the workspace version to $v and publish it." | grep -o '[0-9]*$')
git checkout -q -b "$n-release-$v"
sed -i "/^\[workspace.package\]/,/^\[/ s/^version = \".*\"/version = \"$v\"/" Cargo.toml
cargo build -p yeda
git commit -qam "Release $v (#$n)"
git push -q -u origin HEAD
gh pr create --title "Release $v" --body "Closes #$n"
# CI may not have registered its checks right after the PR opens.
sleep 10
gh pr checks --watch --fail-fast
gh pr merge --squash --delete-branch
git checkout -q main
git pull -q --ff-only

# --allow-dirty: the reader's dist/ and Tauri's schemas are build outputs git ignores.
cargo build -p yeda
cargo publish --workspace --exclude yeda-android --allow-dirty
git tag "v$v"
git push -q origin "v$v"
gh release create "v$v" --generate-notes
