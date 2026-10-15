# yeda-tui

The terminal interface. The binary is `yeda`.

```sh
yeda [library dir]
yeda import-zotero <zotero data dir> [library dir]
yeda import-table <table name> <file.csv> [library dir]
```

`import-zotero` imports the items, annotations, child notes (as `notes/<key>.md`), tag colours and saved searches that aren't in the library yet. It reads a copy of the database, so Zotero can stay open.

PDFs open in yeda-reader: the one next to the `yeda` binary, else the one on `PATH`, unless `reader` in `yeda.toml` names another command. Annotation extraction and image rendering use poppler (`pdftotext`, `pdftoppm`, `pdfinfo`).

## Keys

| Key | Action |
|---|---|
| `j` `k` `Ctrl-d` `Ctrl-u` `gg` `G` | Move |
| `h` `l` `Tab` | Switch column: library, references, right column |
| `c` | Show only the current column; again to show all |
| `/` | Search; in the library column, filter it |
| `f` | Filter by field in a popup (edits the same query as `/`) |
| `s` | Sort in a popup; choosing the current field again reverses it |
| `Esc` | Clear the search |
| `a` | Add by DOI, arXiv ID or ISBN |
| `o` | Open the PDF in yeda-reader |
| `e` / `n` | Edit the reference file / its Markdown note in `$EDITOR` |
| `t` / `m` | Edit tags / move to a collection |
| `r` / `R` | Next / previous reading status |
| `A` / `T` | Show annotations / tables in the right column |
| `x` | Extract the annotations embedded in the PDF |
| `C` | Colour the selected tag (library column) |
| `dd` | Delete the reference or saved search |
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
| `:help`, `:keys` | List every key and command |
