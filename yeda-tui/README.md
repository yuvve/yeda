# yeda-tui

The terminal interface. The binary is `yeda`.

```sh
yeda [library dir]                     # open a library
yeda init                              # create a library
yeda import                            # create a library from Zotero
yeda read <file.pdf> [--annotations <dir>] [--page N]
yeda import-table <table> <file.csv> [library dir]
yeda import-zotero <zotero data dir> [library dir] [--pdfs-to <location>]
yeda copy-pdfs <from location> <to location> [library dir]
```

Without a library dir, yeda opens the current folder if it is a library, else the default library.

## Getting started

`yeda init` creates an empty library. A form asks for its folder, where its PDFs go (a local folder, a WebDAV folder, or none yet), and whether it becomes the default library. The library is ready for git and Syncthing.

`yeda import` creates a library from Zotero's. The same form also asks for Zotero's data folder. It imports the items, annotations, notes, tag colours and saved searches, and copies every PDF to the chosen place, whether Zotero stored it, linked it, or only has it on its WebDAV server. Zotero can stay open; it is only read.

`import-zotero` and `copy-pdfs` are the two steps of `yeda import`, one at a time.

`import-table` fills a table from a CSV file, matching rows by `key` or else `title`. CSV columns the table doesn't have are left out.

## Settings

`~/.config/yeda/config.toml` holds this device's settings; yeda creates it with them commented out:

```toml
library = "~/yeda"               # the default library
reader = "default"               # yeda's own PDF reader

[locations]                      # this device's folder for a location
pdfs = "/mnt/papers"
```

`reader` can name another command, run as `<command> <pdf> --annotations <dir>`. yeda then sees only the annotations that reader saves in the PDF, through `x`.

The library's `yeda.toml` holds the LLM profiles and the literature tables they fill:

```toml
default_llm = "claude"

[llm.claude]
provider = "anthropic"           # or "openai" for any OpenAI-compatible server
model = "claude-opus-5-5"

[tables.lit]
columns = ["contribution", "group", "subproblem"]

[tables.lit.describe]            # optional: what each column should hold
contribution = "What the paper adds, in plain words; no jargon."
group = "The topic it works on: a short label many papers share."
subproblem = "The problem it tackles and why earlier work falls short."
```

A setting in the wrong file is reported, naming the right one. Extracting annotations from a PDF uses poppler (`pdftotext`, `pdftoppm`, `pdfinfo`).

## Secrets

When a password or API key is missing, yeda asks for it and offers to save it in the secret store; `:secrets` lists and changes them. An LLM key can also come from an environment variable: `api_key_env` in the profile, by default `ANTHROPIC_API_KEY` or `OPENAI_API_KEY`.

## Keys and commands

Every key runs a command, and every action is a command (`:annotations` is the same as `A`). `?` lists the keys and commands in effect; `:help` is the guide to searching, tables, graphs, fetching and the reader. Keys are rebound in `config.toml` under `[keys.<context>]`:

```toml
[keys.normal]
"F" = "cite ieee"
"x" = ""                         # unbinds x
```
