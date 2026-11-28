# yeda-tui

<!-- embed -->
<p align="center"><b>A terminal reference manager whose library is plain TOML and Markdown in git.</b><br>
⌨️ Vim keys · 🖍️ Zotero-compatible PDF annotations · 🕸️ citation graphs · 📊 LLM literature tables · 🔎 paper discovery · 📱 Android companion app</p>

![yeda's main window](screenshots/main.png)

yeda is a vibe-coded alternative to Zotero for the terminal. Your library is a folder of plain files that syncs through git or Syncthing; PDFs live in a local folder, on WebDAV or in Dropbox. An Android app reads and annotates the same library.

```sh
cargo install yeda    # or download the binary from the latest release
yeda init             # create a library, or `yeda import` one from Zotero
yeda                  # open it
```

## Read and annotate
![Reader](screenshots/reader.png)

A PDF reader based on Zotero's: highlights, underlines, notes, text, image areas and ink, saved in the library next to the paper, so the Android app shows them too. Column mode reads two-column papers one column at a time.

## Follow the citations
![Citation graph](screenshots/graph.png)

`:graph` lays out what the selected papers cite and what cites them, by year, from OpenAlex. Papers outside the library can be added from it. `:graph tags` and `:graph col <column>` cluster papers by tags or by a table column.

## Compare papers in a table
![Literature table](screenshots/table.png)

One row per paper, one column per question, filled in by an LLM (`:summarize`). The LLM reuses values other papers already have, so a column like `group` sorts papers into shared topics. Tables export to CSV (`:export`) and import back (`yeda import-table`).

## Discover papers
![Describe what to look for](screenshots/discover-query.png)

![Discovery results with relevance notes](screenshots/discover-results.png)

`:discover` searches the web and arXiv for papers matching a description or similar to the marked ones. Review the model's relevance notes and add the papers you want to keep.

## Ask in plain words
![A :do plan](screenshots/do.png)

`:do` turns a request like "add arXiv 2006.11239 to phd/related-work, tag it diffusion and fetch its PDF" into yeda commands. You approve the plan before it runs, and one undo takes all of it back.

## And more

- **Collect:** by DOI, arXiv ID or ISBN (`:add`), by hand (`:new`), or from Zotero with annotations, notes, tag colours and saved searches (`yeda import`).
- **Organize:** collections, tags, reading status, search and saved searches.
- **Fetch:** PDFs from open access or through your library's proxy (`:fetch`).
- **Cite:** BibTeX or a citation style (`:cite`).
- **Sync:** the library through git (`:sync`).
- **Script:** most commands run without the TUI and pipe keys: `yeda search coll:thesis | yeda cite - --format apa`.

Every action is a command with a rebindable key: [keys and commands](KEYS.md), [the reader's keys](../yeda-reader/KEYS.md). Inside yeda, `?` lists them and `:help` is the guide. LLM providers, secrets, PDF locations and the library format are in [CONFIG.md](../CONFIG.md).

## Requirements
yeda runs on Linux only; macOS and Windows have no secret store backend yet ([#292](https://github.com/yuvve/yeda/issues/292)).

- To build: Rust and [Tauri's prerequisites](https://v2.tauri.app/start/prerequisites/).
- To run: WebKitGTK 4.1 (the reader), poppler (full-text search, annotation extraction, summaries), git (sync), a Secret Service provider such as GNOME Keyring, KWallet or KeePassXC (passwords and API keys), and glibc 2.34 or later for the release binary.
<!-- /embed -->
