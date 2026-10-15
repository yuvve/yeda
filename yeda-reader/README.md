# yeda-reader

A desktop PDF reader and annotator built on Tauri v2 and a fork of [zotero/reader](https://github.com/zotero/reader). The fork is a submodule at `reader/`.

```sh
yeda-reader <file.pdf> [--annotations <dir>] [--page N]
```

Annotations are stored in `<dir>/annotations.toml`, merged by `id`. Image and ink annotations also get a PNG in `<dir>/img/`. Without `--annotations`, the folder is `<file.pdf>.annotations/`. yeda-tui passes `annotations/<citekey>` of the library.

## Building

```sh
npm ci                                  # at the repository root
npm run build:reader -w yeda-reader     # builds zotero/reader once
npm run build -w yeda-reader            # bundles the frontend into dist/
cargo run -p yeda-reader -- <file.pdf>
```

`src/` holds the TypeScript glue: creating the reader (`reader.ts`), annotation storage (`annotations.ts`) and the vim keymap (`vim.ts`). `src-tauri/` holds the Tauri commands, which yeda-android also registers.

## Keys

| Key | Action |
|---|---|
| `j` `k` | Scroll |
| `J` `K` `Space` `Shift-Space` | Next / previous page |
| `Ctrl-d` `Ctrl-u` | Half a page down / up |
| `gg` `G` `NG` | First / last / Nth page |
| `/` `n` `N` | Search, next / previous result |
| `+` `-` `=` | Zoom in / out / reset |
| `o` `a` | Toggle outline / annotations sidebar |
| `1t`–`7t` | Highlight, underline, note, text, image, ink, eraser |
| `Esc` | Back to selection |
| `dd` `c` `y` | Delete the selected annotation / edit its comment / copy the selected text |
