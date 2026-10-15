# yeda

A reference manager whose library is plain TOML and Markdown files, kept in git.

| Component | What it is |
|---|---|
| [yeda-core](yeda-core) | Library format and business logic; does no I/O |
| [yeda-net](yeda-net) | Metadata lookup (DOI, arXiv, ISBN) and LLM requests |
| [yeda-fs](yeda-fs) | Loads a library from a local folder and applies changes to it |
| [yeda-tui](yeda-tui) | Terminal interface with vim keybindings |
| [yeda-reader](yeda-reader) | Desktop PDF reader and annotator |
| [yeda-android](yeda-android) | Android app: library browser with the reader built in |

## Library

```
<library>/
  yeda.toml                               # shared settings
  refs/<citekey>.toml                     # one reference each
  notes/<citekey>.md                      # notes
  annotations/<citekey>/annotations.toml  # annotations, merged by id
  annotations/<citekey>/img/<id>.png      # image and ink annotations
```

`yeda.toml`:

```toml
reader = "zathura"               # opens PDFs as <reader> <pdf> --annotations <dir>; default: yeda read
default_llm = "claude"

[locations]                      # pdf.location in a reference names one of these
zotero = "~/Zotero/storage"

[tag_colors]
"priority 1" = "#ff6666"

[searches]
"To read" = 'status:"^To Read$"'

[tables.lit]                     # literature table, filled by an LLM
columns = ["method", "hardware/sim"]
describe = { method = "Algorithm family, a few words." }

[llm.claude]
provider = "anthropic"           # or "openai" for any OpenAI-compatible server
model = "claude-opus-5-5"
```

Settings for one machine go in `~/.config/yeda/config.toml`, which uses the same keys as `yeda.toml` and overrides it. On Android, the app's settings play this role.

API keys are never stored in the library. Clients read the key from the environment variable named by `api_key_env`, which defaults to `ANTHROPIC_API_KEY` or `OPENAI_API_KEY`.

## Building

```sh
git clone --recursive <this repo>
cargo build --workspace
cargo test --workspace
cargo install --path yeda-tui      # the `yeda` binary, with the reader built in
```

Building yeda-reader (and so yeda-tui and yeda-android, which link it) runs npm to build its web frontend. The first time, this installs packages and builds zotero/reader, so it needs Node.js and network access.

The repository is AGPL-3.0, as required by the zotero/reader fork it embeds.
