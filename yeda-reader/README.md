# yeda-reader

A desktop PDF reader and annotator with vim keys, built on Tauri v2 and a fork of [zotero/reader](https://github.com/zotero/reader). The fork is a submodule at `reader/`.

```sh
yeda-reader <file.pdf> [--annotations <dir>] [--page N]
```

Annotations are stored in `<dir>/annotations.toml`, merged by `id`. Image and ink annotations also get a PNG in `<dir>/img/`. Without `--annotations`, the folder is `<file.pdf>.annotations/`. yeda-tui passes the PDF's `annotations/<citekey>/<pdf id>` of the library.

## Building

```sh
cargo run -p yeda-reader -- <file.pdf>
```

The build runs npm to build the web frontend, so it needs Node.js. yeda links this crate: `yeda read` is the same reader, and its keys are listed in yeda's `:help`. It also holds the browser window behind yeda's `:fetch`.
