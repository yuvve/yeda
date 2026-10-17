# Notes for coding agents

## Architecture
- **yeda-core** holds the business logic. It takes text or bytes and returns values or `Effect`s, and never touches the disk, the network, environment variables or subprocesses: `grep -rn "std::fs\|ureq\|Command::\|env::var" yeda-core/src` must find nothing.
- **yeda-net** does all networking and never touches the disk.
- **yeda-fs** does client-side disk I/O and applies `Effect`s atomically.
- **The clients** (yeda-tui, yeda-reader, yeda-android) do all other I/O.
- **The library** stays plain TOML and Markdown that works in git; see the README.
- **Secrets** (passwords, API keys, tokens) live only in the system secret store, through yeda-secrets, never in a file.
- **No backward compatibility:** yeda is a personal project. Replace old formats and behaviour outright, with no compatibility modes or migrations, and say what the user has to redo.

## Testing
- `cargo test --workspace`; `npx tsc --noEmit` in yeda-android.
- CI runs `cargo clippy --workspace --all-targets -- -D warnings` on the latest stable Rust, which may be newer than the local default; run it with that version (`rustup toolchain install <version>`, then `cargo +<version> clippy …`).
- APK:
  ```sh
  cd yeda-android && npm run build
  ANDROID_HOME=~/Android/Sdk NDK_HOME=~/Android/Sdk/ndk/<version> npx tauri android build --apk --target aarch64 --debug --ci
  ```
- Run GUI windows only on a virtual display (`env -u WAYLAND_DISPLAY DISPLAY=:99 GDK_BACKEND=x11`), never on the user's desktop or clipboard.
- Block the secret store in tests with `DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent`. Ask before using a real one.
- Work on a copy of the user's library, with `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `XDG_CACHE_HOME` pointed at a scratch folder.
- Stop processes by exact PID. A `pkill -f` pattern matches the shell running it.
- On the user's phone, send input only while yeda, the file picker or a permission dialog is in front.

## Workflow
- Every change starts from a GitHub issue. Work on a branch `<issue number>-<short-name>` and open a PR that says `Closes #<n>`.
- Keep no TODO or plan files in the repository; plans go in the issue.
