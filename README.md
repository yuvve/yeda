# yeda

A reference manager whose library is plain TOML and Markdown files, kept in git.

| Component | What it is |
|---|---|
| [yeda-core](yeda-core) | Library format and business logic; does no I/O |
| [yeda-net](yeda-net) | Metadata lookup (DOI, arXiv, ISBN), LLM requests and WebDAV transfers |
| [yeda-fs](yeda-fs) | Loads a library from a local folder and applies changes to it |
| [yeda-secrets](yeda-secrets) | Passwords and API keys in the system secret store (Secret Service, Android Keystore) |
| [yeda-tui](yeda-tui) | Terminal interface with vim keybindings |
| [yeda-reader](yeda-reader) | Desktop PDF reader and annotator |
| [yeda-android](yeda-android) | Android app: library browser with the reader built in |

## Library

```
<library>/
  yeda.toml                               # shared settings
  refs/<citekey>.toml                     # one reference each
  notes/<citekey>.md                      # notes
  notes/<citekey>.ai.toml                 # what an LLM wrote: its row in each table
  annotations/<citekey>/annotations.toml  # annotations, merged by id
  annotations/<citekey>/img/<id>.png      # image and ink annotations
```

`yeda.toml`:

```toml
reader = "zathura"               # opens PDFs as <reader> <pdf> --annotations <dir>; default: yeda read
default_llm = "claude"

[locations]                      # pdf.location in a reference names one; PDFs go in <citekey>/<file>
zotero = "~/Zotero/storage"

[locations.dav]                  # a WebDAV server; PDFs are plain files at <citekey>/<file>
url = "https://dav.example.com/zotero/"
user = "yuvve"

[tag_colors]
"priority 1" = "#ff6666"

[searches]
"To read" = { query = 'status:"^To Read$"', sort = "year", descending = true }

[tables.lit]                     # literature table, filled by an LLM
columns = ["contribution", "group", "subproblem"]

[tables.lit.describe]            # optional guidance per column
contribution = "What the paper adds, in plain words; no jargon."
group = "The topic it works on: a short label many papers share."
subproblem = "The problem it tackles and why earlier work falls short."

[llm.claude]
provider = "anthropic"           # or "openai" for any OpenAI-compatible server
model = "claude-opus-5-5"
```

Data is everything in the library folder except PDFs; it syncs through git or Syncthing. PDFs sync through WebDAV or Syncthing. A library folder shared with Syncthing (it has `.stfolder`) gets a `.stignore` that leaves out `.git` and yeda's temporary `*.tmp` files; a Syncthing-shared PDF location gets one that leaves out `*.tmp`. An existing `.stignore` is kept.

Settings for one machine go in `~/.config/yeda/config.toml`, which uses the same keys as `yeda.toml` and overrides it. It also takes `library`, the folder yeda opens by default. yeda creates it on first start, with these settings commented out. On Android, the app's settings play this role.

Secrets are never stored in a file. WebDAV passwords and LLM API keys are kept in the system secret store (see [yeda-secrets](yeda-secrets)), and clients ask for them when missing. On the desktop, an LLM key can also come from the environment variable named by `api_key_env`, which defaults to `ANTHROPIC_API_KEY` or `OPENAI_API_KEY`.

PDFs live at `<location>/<citekey>/<file>`, locally or on WebDAV. A WebDAV PDF is cached (`~/.cache/yeda/<location>/…` on the desktop) and downloaded again when the server's copy changes.

## Building

```sh
git clone --recursive <this repo>
cargo build --workspace
cargo test --workspace
cargo install --path yeda-tui      # the `yeda` binary, with the reader built in
```

Building yeda-reader (and so yeda-tui and yeda-android, which link it) runs npm to build its web frontend. The first time, this installs packages and builds zotero/reader, so it needs Node.js and network access.

The repository is AGPL-3.0, as required by the zotero/reader fork it embeds.
