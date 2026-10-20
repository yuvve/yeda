# yeda

A reference manager, and an alternative to Zotero, built around a terminal interface and a library of plain-text files. The library is a folder of TOML and Markdown that works with git, and PDFs are kept in local or WebDAV folders.

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
