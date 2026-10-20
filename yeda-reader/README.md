# yeda-reader

A desktop PDF reader and annotator with vim keys, built on Tauri v2 and a fork of [zotero/reader](https://github.com/zotero/reader). The fork is a submodule at `reader/`.

```sh
yeda-reader <file.pdf> [--annotations <dir>] [--page N]
```

Annotations are stored in `<dir>/annotations.toml`, merged by `id`. Image and ink annotations also get a PNG in `<dir>/img/`. Without `--annotations`, the folder is `<file.pdf>.annotations/`. yeda-tui passes `annotations/<citekey>` of the library.

## Building

```sh
cargo run -p yeda-reader -- <file.pdf>
```

`build.rs` builds the web frontend into `dist/`, which the binary embeds. If needed it first runs `npm ci` at the repository root and builds zotero/reader. yeda-tui links this crate, so `yeda read` is the same reader, and its keys are listed in yeda's `:help`.

`src-tauri/src/fetch.rs` is the fetch window behind yeda-tui's `:fetch` (`yeda fetch`, a job as JSON on stdin). It opens each paper's DOI through the first library proxy (the Fetch menu moves on to the next) and lets the browser follow the publisher's pages to the PDF and download it. It prints one JSON result per paper. The window keeps its own browser profile, so a proxy sign-in lasts until the proxy session expires.

`src/` holds the TypeScript glue: creating the reader (`reader.ts`), annotation storage (`annotations.ts`) and the vim keymap (`vim.ts`). `src-tauri/` holds the Tauri commands, which yeda-android also registers.
