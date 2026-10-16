# yeda

## The request (verbatim)

> I have 2 repositories here which I want you to refactor: rusty-bib and rusty-pdf.
>
> The refactor I want is:
>
> 1. Change the name to "yeda"
> 2. Do something more akin model-view-control:
>
> - One core library "yeda-core" which handles "business logic" and exposes API. This includes (but not limited to) querying and processing data.
> - "yeda-reader": a pdf reader and annotator (essentially rusty-pdf)
> - "yeda-TUI": a terminal user interface (essentially rusty-tui)
> - "yeda-android": a full-featured ("equally featured" to the TUI app) mobile application with a GUI.
>
> It is of foremost importance to keep the system based on markdown and toml files, but the actual reading and writing to disk should NOT be handled by the core library (the clients should be doing that).

Follow-up: "I prefer the 'magic wand' networking stuff to be its own component, so it will be universal."

Decisions made with the user:
- One monorepo with a Cargo workspace.
- yeda-android is Tauri v2 with a web UI.
- yeda-core does no network I/O. All networking lives in yeda-net.

## Architecture rules

- **yeda-core** holds the business logic: the on-disk format, querying, search, tables, Zotero conversion and annotation merging. It takes text or bytes and returns values or a list of `Effect`s (write, remove, copy, all with library-relative paths). It never touches `std::fs`, the network, environment variables or subprocesses. `grep -rn "std::fs\|ureq\|Command::\|env::var" yeda-core/src` must find nothing.
- **yeda-net** holds all networking: magic-wand metadata lookup (DOI, arXiv, ISBN) and LLM requests. It never touches the disk; PDFs and API keys come in as arguments.
- **yeda-fs** is client-side: it loads a library from a local folder and applies `Effect`s with `std::fs`. All three clients use it. It is not part of core.
- **Clients** (yeda-tui, yeda-reader, yeda-android) do all disk I/O, apply `Effect`s atomically (write a temp file, then rename), read environment variables and run subprocesses.
- yeda-android reuses yeda-reader's Tauri commands and frontend for the PDF view.
- The library stays plain text and works in git. TOML holds data, Markdown holds notes.

## Library format

```
<library>/
  yeda.toml                         # shared: [locations], [tag_colors], [searches], [tables.*], [llm.*], reader
  refs/<citekey>.toml               # one reference
  notes/<citekey>.md                # standalone notes (Markdown)
  annotations/<citekey>/annotations.toml
  annotations/<citekey>/img/<id>.png
```
Machine-local overrides live in `~/.config/yeda/config.toml`, with the same keys as `yeda.toml`; this file wins over the library's.

PDF paths are never hardcoded. `pdf.location` names an entry under `[locations]`, and `pdf.path` is relative to it.

`annotations.toml` holds a list of `[[annotation]]` entries:
```toml
[[annotation]]
id = "ABCD1234"
kind = "highlight"        # highlight | underline | note | text | image | ink
page = "5"                # page label as displayed
color = "#ffd400"
text = "highlighted text"
comment = "my comment"
image = "img/ABCD1234.png" # image and ink only
source = "reader"          # zotero | pdf | reader
sort_index = "00004|001234|00056"

[annotation.position]     # Zotero's position JSON, keys kept verbatim
pageIndex = 4
rects = [[49.5, 41.1, 568.2, 210.3]]
```
- Fields map 1:1 to zotero/reader's annotation objects. Only the names differ: `kind` is Zotero's `type`, and `sort_index` is `sortIndex`.
- Writers re-read the file, merge by `id` and write atomically. They never drop entries or fields they don't understand.
- Image and ink annotations also get a PNG at `img/<id>.png`.

## Features to keep (from rusty-bib)

1. Plain-text library that works with git.
2. PDF locations are named folders, never hardcoded paths.
3. Collections and tags, with tag colours.
4. Native vim keybindings.
5. Opening PDFs in yeda-reader, or in the system viewer.
6. Annotation preview without opening the PDF.
7. Import from Zotero, preserving metadata and annotations.
8. A "magic wand" that adds a reference from a DOI, arXiv id or ISBN.
9. Fuzzy and field-based search, plus saved searches.
10. Reading status, and LLM-filled literature tables (CSV import and export).

yeda-android must reach parity with yeda-tui. The exception is Zotero import, which needs a Zotero sqlite file and stays desktop-only. Extracting annotations embedded in a PDF uses poppler on desktop and pdf.js on Android.

## yeda-reader (from rusty-pdf)

- A PDF reader with Zotero's annotation tools: highlight, underline, note, text, image (area), ink and eraser.
- Search inside the document, and vim keybindings throughout.
- Tauri v2 shell around a fork of [zotero/reader](https://github.com/zotero/reader), a submodule at `yeda-reader/reader`. Fork changes stay minimal and on a branch. Because of zotero/reader, the repo is AGPL-3.0.
- Launch: `yeda-reader <file.pdf> [--annotations <dir>] [--page N]`. Without `--annotations`, annotations go in a sidecar folder `<file>.annotations/`.
- Vim keymap:
  - Movement: `j`/`k`, `J`/`K`, `Space`/`Shift-Space`, `Ctrl-d`/`Ctrl-u`, `gg`/`G`, `NG`.
  - Search: `/`, `n`/`N`.
  - View: `+` `-` `=` zoom, `o` outline, `a` annotations sidebar.
  - Tools: `1t`–`7t`, `Esc` back to selection.
  - Selected annotation: `dd` deletes it, `c` edits its comment, `y` copies the text.
  - Later: visual mode (`v`, then `w`/`b`/`e`, then `h`).

## yeda-android

- Runs on Android e-ink devices with a pen. The target device is a ViWoods AiPaper with an EMR stylus.
- A library browser (collections tree, tags in their colours, search, every yeda-tui action) with yeda-reader embedded for PDFs.
- The library folder is chosen with the system folder picker and accessed with all-files access. The app also registers for "Open with" on PDFs, using the sidecar folder.
- Sync: Syncthing on Android, git on desktop.

## Later phases

Open work is listed in `TODO.md`. Done so far:
- WebDAV attachments in Zotero's layout: download on open, `:attach` uploads. The URL and user name go in `yeda.toml`; passwords go in the system secret store via yeda-secrets, never in files.
- Git sync: `:sync` in yeda-tui uses the git CLI; yeda-android uses libgit2 over HTTPS with a token in the Keystore. Both merge conflicting `annotations.toml` files by `id` (`annotations::merge3`).

## Verification

- `cargo test --workspace`.
- **Desktop:** annotate in yeda-reader, check the `annotations.toml` diff, and confirm the annotation, image included, appears in yeda-tui's annotation pane (`A`).
- **Android:** `adb install` the CI APK and watch `adb logcat`. Check the library browser features against yeda-tui, then pen drawing, the side-button eraser, palm rejection and e-ink mode.
