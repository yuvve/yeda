# yeda-tui

The terminal interface. The binary is `yeda`.

```sh
yeda [library dir]
yeda read <file.pdf> [--annotations <dir>] [--page N]
yeda import-zotero <zotero data dir> [library dir] [--pdfs-to <location>]
yeda import-table <table name> <file.csv> [library dir]
yeda copy-pdfs <from location> <to location> [library dir]
```

When a WebDAV password or LLM API key is needed and not in the Secret Service, yeda asks for it in a masked prompt and offers to save it there. If the server rejects a saved password, it asks again.

`import-zotero` imports the items, annotations, child notes (as `notes/<key>.md`), tag colours and saved searches that aren't in the library yet. It reads a copy of the database, so Zotero can stay open.

`copy-pdfs` copies every PDF at one location to another (a local folder or WebDAV) as `<citekey>/<file>`, and points the references at the copies; the originals stay. From the `zotero` location, a PDF missing on disk is downloaded from Zotero's WebDAV (`<key>.zip`, found through Zotero's preferences), which is only read. `import-zotero --pdfs-to <location>` runs `copy-pdfs zotero <location>` after importing.

yeda-reader is linked into the binary: `yeda read` runs it, and `o` opens PDFs with it unless `reader` in `yeda.toml` names another command. Building yeda-tui builds the reader too. Annotation extraction and image rendering use poppler (`pdftotext`, `pdftoppm`, `pdfinfo`).

## Keys

These are the defaults from [`src/keys.toml`](src/keys.toml). Each key runs a command line, and every action is also a command (`:annotations`, `:down`). Rebind keys under `[keys.<context>]` in `~/.config/yeda/config.toml`. The contexts are `normal`, plus `library`, `graph`, `table` and `annotations`, which take precedence in their column. `""` unbinds a key. `?` shows the bindings in effect.

```toml
[keys.normal]
"F" = "cite ieee"
"x" = ""
```

| Key | Action |
|---|---|
| `j` `k` `Ctrl-d` `Ctrl-u` `gg` `G` | Move |
| `h` `l` `Tab` | Switch column: library, references, right column |
| `c` | Show only the current column; again to show all. Below 120 terminal columns the right column is hidden, below 80 the library column too |
| Mouse | A click selects and focuses; the wheel moves the selection in the column under the pointer |
| `/` | Search; in the library column, filter it. Spaces mean AND, `\|` means OR, `!` means NOT, `field:value` searches one field |
| `v` / `V` | Mark or unmark the reference / mark a range as you move; `Enter` keeps it and `Esc` cancels it. `Esc` outside a range unmarks all. `r`, `t`, `m`, `dd`, `x` and `:summarize` act on every marked reference |
| `f` | Filter by field in a popup (edits the same query as `/`); values the library has are suggested, Ctrl-N/Ctrl-P pick one and `→` completes it |
| `s` | Sort in a popup; choosing the current field again reverses it |
| `Esc` | Clear the search |
| `a` | Add by DOI, arXiv ID or ISBN; a pasted list adds each, and the ones that fail are listed and copied to the clipboard (`:failed` shows them again) |
| `o` | Open the PDF in the reader |
| `e` / `n` | Edit the reference file / its Markdown note in `$EDITOR` |
| `t` / `m` | Edit tags / move to a collection |
| `r` / `R` | Next / previous reading status |
| `A` | Show annotations in the right column; there, `o` opens an image in the image viewer and any other annotation in `$EDITOR` |
| `T` | `:table` |
| `x` | Extract the annotations embedded in the PDF |
| `C` | Colour the selected tag (library column) |
| `dd` | Delete the reference or saved search |
| `?` | List every key and command; `/` searches it, `n`/`N` move between matches, as in `:help` |
| `:` | Command; Up/Down recall earlier ones, Tab completes the command or its argument (shift-Tab and Tab cycle through several) |
| `q` | Close the graph if it's open, else quit |

## Commands

| Command | Action |
|---|---|
| `:log` | Open the log of status messages and failures (`~/.local/state/yeda/yeda.log`) in `$EDITOR` |
| `:save <name>` | Save the current search |
| `:table [name]` | A table of the selected or marked references in the right column; without a name, the only table or one picked from a list: `h`/`l` pick a column, `Enter` shows a cell in full, `e` edits it in `$EDITOR`, `s` sorts, `/` searches and `f` filters the column; `Esc`, `q` or `:table close` closes it |
| `:summarize[!] [table] [profile]` | Fill the selected reference's table row with an LLM, kept in `notes/<key>.ai.toml`, following the table's `prompt` and `describe`; without a table name, pick one from a list; `!` overwrites filled cells |
| `:summarize-all[!] [table] [profile]` | The same for every visible reference |
| `:cancel` | Stop summarizing after the current paper |
| `:export <table> <file.csv>` | Export a table |
| `:attach <file.pdf> [location]` | Attach a PDF to the selected reference: copy it to a local location or upload it to a WebDAV one, as `<key>/<file>` |
| `:sync` | Commit all changes, pull with `git pull --no-rebase` and push; conflicting `annotations.toml` files are merged by annotation `id`, any other conflict aborts the merge |
| `:secrets` | Every password and API key the configuration uses (WebDAV locations, LLM profiles, OpenAlex), with where each is set; Enter sets or replaces one, `d` removes it. A secret a feature asks for is saved to the secret store |
| `:graph tags`, `:graph col <column>` | The selected or marked references with their tags, or a table column's values (`:graph col group`), clustered: papers sharing a value sit around it. `Enter` selects the picked node, `Backspace` goes back, `Tab` lists every node |
| `:graph[!] [citations]` | Citation graph of the selected (or marked) references, from OpenAlex; see `:help`. `!` fetches citing papers again and retries what OpenAlex didn't have. An optional OpenAlex API key is set with `:secrets`. `:graph close` (or `q`, or Esc in the graph) closes it |
| `:cite [format]` | Copy the selected or marked references to the clipboard as `bibtex` (the default, or `[cite] format`), `ident` (one DOI, arXiv ID, ISBN or URL per line), a citation style shipped with hayagriva (`ieee`, `apa`, `chicago-author-date`, …), or a `.csl` file |
| `:fetch [location]` | Find PDFs for the selected or marked references and attach them to `location` (default `[fulltext] location`): open-access copies first, then the publisher through the library proxy in a browser window; see `:help` |
| `:keys` | List every key and command |
| `:help` | Guide to search syntax (AND, OR, NOT, fields), filtering, sorting and saved searches |
