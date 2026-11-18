# Test library

A small library of real papers, for tests, manual runs and screenshots. Everything here is
plain files in yeda's library format: 27 papers, sample notes, one filled literature table and
two annotations.

PDFs are not in the repository. The six open-access ones are listed in `pdfs.tsv`, and
`scripts/test-library.sh` fetches them into a scratch copy:

```sh
scripts/test-library.sh                # ${TMPDIR:-/tmp}/yeda-test-library
scripts/test-library.sh path/to/copy
```

The copy gets a device config that points the `papers` location at the fetched PDFs, and the
script prints the command that opens it. It refuses to write into a folder that is not empty.

`cargo test -p yeda-fs --test library` loads this library and fails if a reference, note, table
row or annotation stops parsing, or if a saved search finds nothing. The test counts the
references, so update that number when you add one.

## Adding a paper

```sh
yeda add <DOI> --lib testdata/library
```

Then give it tags, a collection and a reading status, as the other references have them.
Adding a paper with a PDF also means a line in `pdfs.tsv` and a file name matching
`<citekey>/<citekey>.pdf`.

## Layout

- `yeda.toml`: tag colours, six saved searches, nested collections and the `lit` table.
- `refs/`: one reference per paper.
- `notes/<citekey>.md`: sample reading notes; `notes/<citekey>.ai.toml` holds the table rows.
- `annotations/<citekey>/<pdf id>/annotations.toml`: a highlight and an underline on the
  Transformer paper, positioned on its first page.
