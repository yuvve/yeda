# yeda

**All of yeda's code and documentation is written by AI (Claude). The author directs what it should do and tests it by using it, but has not read or reviewed the code.**

A reference manager, and an alternative to Zotero, built around a terminal interface and a library of plain-text files. The library is a folder of TOML and Markdown that works with git, and PDFs are kept in local or WebDAV folders.

The terminal interface does everything:
- adding references by DOI, arXiv ID or ISBN, and importing a Zotero library;
- collections, tags, reading status, search, filters and saved searches;
- a PDF reader and annotator with vim keys, and Markdown notes;
- finding PDFs, open access or through a library proxy;
- citation graphs of the papers a reference cites and is cited by;
- literature tables: an LLM reads each paper and answers the same questions for all of them, one row per paper;
- finding related papers: an LLM searches for papers like the selected ones, checked against OpenAlex;
- citing as BibTeX or in a citation style, and syncing with git.

The Android app is a companion for reading and annotating: it browses, searches and organizes the library, syncs it with git, and has the same reader, with pen support.

| Component | What it does |
|---|---|
| [yeda-tui](yeda-tui) | The `yeda` command: the terminal interface with vim keybindings, plus creating a library and importing one from Zotero |
| [yeda-reader](yeda-reader) | Desktop PDF reader and annotator, built into `yeda` |
| [yeda-android](yeda-android) | Android app for reading and annotating: library browser with the reader built in |
| [yeda-core](yeda-core) | Library format and logic; does no I/O |
| [yeda-net](yeda-net) | Metadata lookup, LLM requests and WebDAV transfers |
| [yeda-fs](yeda-fs) | Reads a library folder and writes changes to it |
| [yeda-secrets](yeda-secrets) | Passwords and API keys in the system secret store |

## The library

```
<library>/
  yeda.toml                               # settings shared by every device
  refs/<citekey>.toml                     # one reference each
  notes/<citekey>.md                      # your notes
  notes/<citekey>.ai.toml                 # what an LLM wrote for the tables
  annotations/<citekey>/annotations.toml  # annotations
  annotations/<citekey>/img/<id>.png      # image and ink annotations
```

PDFs are kept outside the library folder, in *locations*: local folders or WebDAV folders, each holding `<citekey>/<file>`. WebDAV PDFs are cached locally.

## Settings

`yeda.toml` holds what every device shares, such as the locations and saved searches:

```toml
[locations]
pdfs = "~/yeda-pdfs"             # a local folder
dav = { url = "https://dav.example.com/yeda/", user = "me" }

[searches]
"To read" = { query = 'status:"^To Read$"', sort = "year", descending = true }
```

Each device also has its own config, which may give a location a different folder on that device: `~/.config/yeda/config.toml` on the desktop (see [yeda-tui](yeda-tui)), the app's settings on Android.

All passwords and API keys are kept in the system secret store.

## Building

```sh
git clone --recursive <this repo>
cargo install --path yeda-tui      # the `yeda` binary, with the reader built in
```

The first build needs Node.js and network access, to build the reader's web frontend. The repository is AGPL-3.0, as required by the zotero/reader fork it embeds.
