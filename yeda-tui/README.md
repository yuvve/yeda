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

Without a library dir, yeda opens the current folder if it is a library, else `library` from `~/.config/yeda/config.toml`.

## Getting started

`yeda init` asks in a form for the library folder, where its PDFs go (a local folder, a WebDAV folder, or none yet) and whether to make it the default library.

`yeda import` does the same for a Zotero library, read from Zotero's data folder: items, annotations, notes, tag colours and saved searches. It copies every PDF to the chosen place, whether Zotero stored it, linked it, or only has it on its WebDAV server. Zotero can stay open; it is only read.

`import-zotero` and `copy-pdfs` are the two steps of `yeda import`, one at a time.

`import-table` fills a table from a CSV file, matching rows by `key` or else `title`. CSV columns the table doesn't have are left out.

## Reading

`o` opens a PDF in yeda's reader (`yeda read`, the same as [yeda-reader](../yeda-reader)), or in the command `reader` names in the device config. Extracting annotations from a PDF uses poppler (`pdftotext`, `pdftoppm`, `pdfinfo`).

## Secrets

When a password or API key is missing, yeda asks for it and offers to save it in the secret store; `:secrets` lists and changes them. An LLM key can also come from an environment variable: `api_key_env` in the profile, by default `ANTHROPIC_API_KEY` or `OPENAI_API_KEY`.

## Keys

The defaults are in [`src/keys.toml`](src/keys.toml); `?` shows the bindings in effect. Each key runs a command, so keys are rebound in the device config, `~/.config/yeda/config.toml`, under `[keys.<context>]`: `normal`, or `library`, `graph`, `table` or `annotations` for that column only. `""` unbinds a key.

```toml
[keys.normal]
"F" = "cite ieee"
"x" = ""
```

| Key | Action |
|---|---|
| `j` `k` `Ctrl-d` `Ctrl-u` `gg` `G` | Move |
| `h` `l` `Tab` | Switch column: library, references, right column |
| `c` | Show only the current column; again to show all. Narrow terminals hide the right column, then the library column |
| Mouse | Click selects; the wheel scrolls the column under the pointer |
| `/` | Search (in the library column: filter it). Spaces mean AND, `\|` OR, `!` NOT, `field:value` one field; `:help` has the rest |
| `v` / `V` | Mark the reference / mark a range as you move (`Enter` keeps it, `Esc` cancels). `V G` marks the whole list. Changing or deleting several asks first. `Esc` unmarks all |
| `f` | Filter by field in a popup, with the library's values suggested: Ctrl-N/Ctrl-P pick, Tab completes, Ctrl-D/Ctrl-U move between boxes, Ctrl-L clears one, Ctrl-X all |
| `s` | Sort in a popup; choosing the current field again reverses it |
| `Esc` | Clear the search |
| `a` | Add by DOI, arXiv ID or ISBN; a pasted list adds each, and `:failed` lists the ones that failed |
| `o` | Open the PDF in the reader |
| `e` / `n` | Edit the reference file / its Markdown note in `$EDITOR` |
| `t` / `m` | Edit tags / move to a collection |
| `r` / `R` | Next / previous reading status |
| `A` | Annotations in the right column; `o` opens one |
| `T` | `:table` |
| `x` | Extract the annotations embedded in the PDF |
| `C` | Colour the selected tag (library column) |
| `dd` | Delete the reference or saved search |
| `?` | Every key and command; `/` searches, `n`/`N` move between matches |
| `:` | Command line; Up/Down recall, Tab completes |
| `q` | Close the graph if it's open, else quit |

## Commands

A command acts on the selected reference, or on the marked ones.

| Command | Action |
|---|---|
| `:log` | Open the log of messages and failures in `$EDITOR` |
| `:save <name>` | Save the current search |
| `:table [name]` | The table for the selected or marked references, in the right column: `h`/`l` pick a column, `Enter` shows a cell, `e` edits it, `s` sorts, `/` searches, `f` filters; `q` closes |
| `:table rename <old> <new>` | Rename a table |
| `:summarize[!] [table] [profile]` | Fill the table row with an LLM, following the table's `describe`; `!` overwrites filled cells |
| `:cancel` | Stop summarizing after the current paper |
| `:export <table> <file.csv>` | Export a table |
| `:attach <file.pdf> [location]` | Attach a PDF, copying or uploading it to a location |
| `:git-init [url]` | Make the library a git repository; with a URL, push it there so `:sync` works |
| `:sync` | Commit, pull and push with git. Conflicting annotations are merged; any other conflict stops the sync |
| `:discover [what]` | Ask the LLM for papers like the selected ones, or as described, and look each up in OpenAlex; `a` adds one, `o` opens it, `:found` shows the list again. Searching the web needs an Anthropic profile |
| `:setup llm\|webdav [name]` | A form for an LLM profile (←/→ picks a provider and fills in its URL) or a WebDAV location. Enter tests it, then saves |
| `:setup openalex` | Set the optional OpenAlex API key, which raises `:graph`'s daily budget |
| `:secrets` | Every password and API key in use: Enter sets one, `d` removes it |
| `:graph tags`, `:graph col <column>` | The selected references clustered by tag, or by a table column's value |
| `:graph[!]` | Citation graph of the selected references, from OpenAlex; see `:help`. `!` fetches again. `q` closes it |
| `:cite [format]` | Copy the references as `bibtex` (default), `ident` (DOI, arXiv ID, ISBN or URL), a style such as `ieee` or `apa`, or a `.csl` file |
| `:fetch [location]` | Find PDFs and attach them: open-access copies first, then the publisher through the library proxy in a browser window; see `:help` |
| `:help` | Guide to searching, filtering, sorting, graphs and fetching |
