# yeda

A reference manager whose library is a folder of plain TOML and Markdown files.

| Component | What it is |
|---|---|
| [yeda-tui](yeda-tui) | Terminal interface with vim keybindings; start here |
| [yeda-reader](yeda-reader) | Desktop PDF reader and annotator |
| [yeda-android](yeda-android) | Android app: library browser with the reader built in |
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

`yeda.toml`, in the library, holds the settings every device shares:

```toml
default_llm = "claude"

[locations]
pdfs = "~/yeda-pdfs"             # a local folder
dav = { url = "https://dav.example.com/yeda/", user = "me" }

[searches]
"To read" = { query = 'status:"^To Read$"', sort = "year", descending = true }

[tables.lit]                     # a literature table, filled in by an LLM
columns = ["contribution", "group", "subproblem"]

[tables.lit.describe]            # optional: what each column should hold
contribution = "What the paper adds, in plain words; no jargon."
group = "The topic it works on: a short label many papers share."
subproblem = "The problem it tackles and why earlier work falls short."

[llm.claude]
provider = "anthropic"           # or "openai" for any OpenAI-compatible server
model = "claude-opus-5-5"
```

The device config, `~/.config/yeda/config.toml`, holds what differs between devices. yeda creates it with these settings commented out:

```toml
library = "~/yeda"               # the library yeda opens by default
reader = "zathura"               # PDF reader command; default: yeda's own

[locations]                      # this device's folder for a location
pdfs = "/mnt/papers"

[keys.normal]                    # key bindings; see yeda-tui
"F" = "cite ieee"
```

A setting in the wrong file is an error that names the right one. On Android, the app's settings are the device config.

Passwords and API keys are never written to a file: they go in the system secret store, and yeda asks for them when they're missing.

## Syncing

Sync the library folder with git (`:sync`) or Syncthing, and the PDFs with WebDAV or Syncthing. `yeda init` and `yeda import` set the library up for either.

## Building

```sh
git clone --recursive <this repo>
cargo install --path yeda-tui      # the `yeda` binary, with the reader built in
```

The first build needs Node.js and network access, to build the reader's web frontend. The repository is AGPL-3.0, as required by the zotero/reader fork it embeds.
