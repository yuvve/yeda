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

| Key | Action |
|---|---|
| `j` `k` `Ctrl-d` `Ctrl-u` `gg` `G` | Move |
| `h` `l` `Tab` | Switch column: library, references, right column |
| `c` | Show only the current column; again to show all |
| `/` | Search; in the library column, filter it. Spaces mean AND, `\|` means OR, `!` means NOT, `field:value` searches one field |
| `v` / `V` | Mark or unmark the reference / mark a range as you move, until `Esc`; `Esc` again unmarks all. `r`, `t`, `m`, `dd`, `x` and `:summarize` act on every marked reference |
| `f` | Filter by field in a popup (edits the same query as `/`); values the library has are suggested, Ctrl-N/Ctrl-P pick one and `→` completes it |
| `s` | Sort in a popup; choosing the current field again reverses it |
| `Esc` | Clear the search |
| `a` | Add by DOI, arXiv ID or ISBN; a pasted list adds each, and the ones that fail are listed and copied to the clipboard (`:failed` shows them again) |
| `o` | Open the PDF in the reader |
| `e` / `n` | Edit the reference file / its Markdown note in `$EDITOR` |
| `t` / `m` | Edit tags / move to a collection |
| `r` / `R` | Next / previous reading status |
| `A` / `T` | Show annotations / tables in the right column |
| `x` | Extract the annotations embedded in the PDF |
| `C` | Colour the selected tag (library column) |
| `dd` | Delete the reference or saved search |
| `?` | List every key and command |
| `:` | Command |
| `q` | Quit |

## Commands

| Command | Action |
|---|---|
| `:save <name>` | Save the current search |
| `:summarize[!] [table] [profile]` | Fill the selected reference's table row with an LLM; `!` overwrites filled cells |
| `:summarize-all[!] [table] [profile]` | The same for every visible reference |
| `:cancel` | Stop summarizing after the current paper |
| `:export <table> <file.csv>` | Export a table |
| `:attach <file.pdf> [location]` | Attach a PDF to the selected reference: copy it to a local location or upload it to a WebDAV one, as `<key>/<file>` |
| `:sync` | Commit all changes, pull with `git pull --no-rebase` and push; conflicting `annotations.toml` files are merged by annotation `id`, any other conflict aborts the merge |
| `:graph[!] [selected\|filtered\|prefetch]` | Citation graph of the selected (or marked) references, the list, or the whole library, from OpenAlex; see `:help`. `!` fetches citing papers again. `:graph key` sets an optional OpenAlex API key, kept in the secret store |
| `:cite [format]` | Copy the selected or marked references to the clipboard as `bibtex` (the default, or `[cite] format`), a citation style shipped with hayagriva (`ieee`, `apa`, `chicago-author-date`, …), or a `.csl` file |
| `:fetch [location]` | Find PDFs for the selected or marked references and attach them to `location` (default `[fulltext] location`): open-access copies first, then the publisher through the library proxy in a browser window; see `:help` |
| `:keys` | List every key and command |
| `:help` | Guide to search syntax (AND, OR, NOT, fields), filtering, sorting and saved searches |
