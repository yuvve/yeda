# Notes for coding agents

## Architecture
- **yeda-core** holds the business logic. It takes text or bytes and returns values or `Effect`s, and never touches the disk, the network, environment variables or subprocesses: `grep -rn "std::fs\|ureq\|Command::\|env::var" yeda-core/src` must find nothing.
- **yeda-net** does all networking and never touches the disk.
- **yeda-fs** does client-side disk I/O and applies `Effect`s atomically.
- **The clients** (yeda-tui, yeda-reader, yeda-android) do all other I/O.
- **The library** stays plain TOML and Markdown that works in git; see CONFIG.md.
- **Storage:** the library files are everything in the library folder (references, annotations and their images, notes, clips, config); PDFs live in locations outside it. Library files sync through git or Syncthing, PDFs through WebDAV, Dropbox or Syncthing. Git never carries PDFs, and WebDAV and Dropbox carry nothing but PDFs.
- **Secrets** (passwords, API keys, tokens) live only in the system secret store, through yeda-secrets, never in a file; an environment variable named after the server's host overrides the stored one.
- **No backward compatibility:** yeda is a personal project. Replace old formats and behaviour outright, with no compatibility modes or migrations, and say what the user has to redo.

## Interface
- A command acts on the selected reference, or on the marked ones when there are any. Don't add a separate command for every reference (like the former `:summarize-all`): marking the whole list (`V G`) covers it.
- A command that needs references acts on the marked ones, else the selected one. A command where references are optional (`:discover`) uses only the marked ones, so the user marks first (`v`, `V`) and then calls it.

## Testing
- `cargo test --workspace`; `npx tsc --noEmit` in yeda-android.
- The Audit workflow runs `cargo audit` and `npm audit` on lockfile changes and weekly; Dependabot opens one grouped update PR a week per ecosystem, and one for the zotero/reader submodule.
- CI runs `cargo clippy --workspace --all-targets -- -D warnings` on the latest stable Rust, which may be newer than the local default; run it with that version (`rustup toolchain install <version>`, then `cargo +<version> clippy …`).
- APK:
  ```sh
  cd yeda-android && npm run build
  ANDROID_HOME=~/Android/Sdk NDK_HOME=~/Android/Sdk/ndk/<version> npx tauri android build --apk --target aarch64 --debug --ci
  ```
- Run GUI windows only on a virtual display (`env -u WAYLAND_DISPLAY DISPLAY=:99 GDK_BACKEND=x11`), never on the user's desktop or clipboard.
- Block the secret store in tests with `DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent`. Ask before using a real one.
- Never run yeda on the user's library. Manual runs and screenshots use the test library: `scripts/test-library.sh` copies `testdata/library` to a scratch folder, fetches its open-access PDFs, writes a device config there and prints the command that opens it with `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_CACHE_HOME` pointed at that folder.
- Stop processes by exact PID. A `pkill -f` pattern matches the shell running it.
- On the user's phone, send input only while yeda, the file picker or a permission dialog is in front.

## Workflow
- Every change starts from a GitHub issue. Work on a branch `<issue number>-<short-name>` and open a PR that says `Closes #<n>`.
- Markdown-only PRs are eligible to merge without CI; other PRs require passing CI. Get explicit maintainer approval before merging either.
- Merge with `gh pr merge --squash --delete-branch`. GitHub deletes the branch on merge too; git can't tell a squash-merged branch is merged, so kept ones only pile up.
- Give every new issue one Eisenhower label, and work in that order, oldest first within a quadrant:
  - `Q1 do first`: urgent and important;
  - `Q2 plan`: important;
  - `Q3 quick`: urgent;
  - `Q4 later`: neither.

  Urgent means broken, blocking the user now, or risking data or secrets. Important means it serves reading and annotating, the library's integrity or security.
- Keep no TODO or plan files in the repository; plans go in the issue.
- README changes go in the PR whose change they describe, not in a later one.
- Maintainers publish to crates.io with `scripts/release.sh <version>`. It bumps the workspace versions in the root `Cargo.toml` through an issue and PR, squash-merges after CI passes, publishes the workspace, and pushes `v<version>`. The tag workflow creates the GitHub release with the APK and Linux binaries. Get explicit maintainer authorization before running the script.
- Tag `android-vYYYY.MM.DD` (add `.2`, `.3` for later builds that day) only when the user asks for a beta; push it on main, or on a PR branch to test before merging. The tag builds the APK and publishes it as a pre-release. Releases build their own APK.

## Code comments
- NEVER write a comment describing what code does. What a function does must be evident from its name and signature; if it isn't, fix the name, don't add a comment. This includes section-marker comments that restate the next line ("# parse the config", "# loop over nodes", "# return result").
- Comments exist for WHY: a non-obvious constraint, a workaround for a bug in a dependency, a deliberate deviation from the obvious implementation, an ordering requirement that isn't visible locally. If there's nothing surprising, write nothing.
- Never leave commentary about the editing process: "changed this to fix X", "added per request", "TODO: consider refactoring".
