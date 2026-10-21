# yeda-core

Part of [yeda](https://github.com/yuvve/yeda), and not meant to be used on its own: install yeda with `cargo install yeda`.

The library format and the logic on top of it. It reads no files, makes no network requests and runs no processes. Callers pass in file contents and get back values or `Effect`s, which are writes and removals at library-relative paths for the caller to apply.

| Module | Contents |
|---|---|
| `library` | `Library::parse` from `yeda.toml`, the device config and `refs/*.toml`; saving, deleting and config edits as effects; PDF path resolution; reading status |
| `browse` | Sidebar entries (saved searches, collections, tags) and the references a filter and query select |
| `search` | Query parsing (`field:value`, `\|` alternatives, `!` negation, `""` for an empty field), fuzzy matching, and composing a query from fields |
| `annotations` | `annotations.toml`: typed parsing, merge and delete by `id`, and extraction of the annotations embedded in a PDF |
| `tables` | LLM prompts for literature tables, parsing the model's reply, and CSV import and export |
| `ids` | Recognising DOIs, arXiv IDs and ISBNs |
| `llm` | LLM profile configuration |
| `graph` | Citation graphs: the seeds, the library and outside works linked to them, and a layout by year |
| `fulltext` | Finding a PDF: open-access and publisher URLs, the library proxy, and checking a PDF is the paper |
| `cite` | BibTeX export and bibliographies in a CSL style (hayagriva) |
| `zotero` | Import from a Zotero database (feature `zotero`) |

Some operations need things core can't provide itself, so the caller passes them in as arguments:

- PDF extraction takes the PDF's bytes, optionally `pdftotext -bbox` output, and a callback that renders a region to PNG.
- Zotero import takes an open `rusqlite::Connection` and a callback that returns an annotation image's PNG.
