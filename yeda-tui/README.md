# yeda-tui

The terminal interface. The binary is `yeda`.

```sh
yeda [library dir]
yeda import-zotero <zotero data dir> [library dir]
yeda import-table <table name> <file.csv> [library dir]
```

`import-zotero` imports the items, annotations, child notes (as `notes/<key>.md`), tag colours and saved searches that aren't in the library yet. It reads a copy of the database, so Zotero can stay open.

Annotation extraction and image rendering use poppler (`pdftotext`, `pdftoppm`, `pdfinfo`).

## Keys

| Key | Action |
|---|---|
| `j` `k` `Ctrl-d` `Ctrl-u` `gg` `G` | Move |
| `h` `l` `Tab` | Switch pane: library, references, right pane |
| `/` | Search; in the library pane, filter it |
| `Esc` | Clear the search |
| `a` | Add by DOI, arXiv ID or ISBN |
| `o` | Open the PDF |
| `e` / `n` | Edit the reference file / its Markdown note in `$EDITOR` |
| `t` / `m` | Edit tags / move to a collection |
| `s` / `S` | Next / previous reading status |
| `A` / `T` | Show annotations / tables in the right pane |
| `x` | Extract the annotations embedded in the PDF |
| `C` | Colour the selected tag (library pane) |
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
