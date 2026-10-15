# yeda-core

The library format and the logic on top of it. It reads no files, makes no network requests and runs no processes. Callers pass in file contents and get back values or `Effect`s, which are writes and removals at library-relative paths for the caller to apply.

| Module | Contents |
|---|---|
| `library` | `Library::parse` from `yeda.toml`, the machine config and `refs/*.toml`; saving, deleting and config edits as effects; PDF path resolution; reading status |
| `browse` | Sidebar entries (saved searches, collections, tags) and the references a filter and query select |
| `search` | Query parsing (`field:value`, `\|` alternatives, `!` negation, `""` for an empty field), fuzzy matching, and composing a query from fields |
| `annotations` | `annotations.toml`: typed parsing, merge and delete by `id`, and extraction of the annotations embedded in a PDF |
| `tables` | LLM prompts for literature tables, parsing the model's reply, and CSV import and export |
| `ids` | Recognising DOIs, arXiv IDs and ISBNs |
| `llm` | LLM profile configuration |
| `webdav` | Zotero's WebDAV layout: packing and unpacking `<key>.zip` and `<key>.prop` |
| `zotero` | Import from a Zotero database (feature `zotero`) |

Some operations need things core can't provide itself, so the caller passes them in as arguments:

- PDF extraction takes the PDF's bytes, optionally `pdftotext -bbox` output, and a callback that renders a region to PNG.
- Zotero import takes an open `rusqlite::Connection` and a callback that returns an annotation image's PNG.

`Effect::Write` must be applied atomically, because several clients write the same files.
