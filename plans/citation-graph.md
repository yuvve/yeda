# Citation graph

A view of how papers cite each other: references out of a paper, citations into it, and how papers connect, both inside the library and outside it. It is modelled on Local Citation Network, and runs in yeda-tui first, then yeda-android.

## Data source: OpenAlex
- `GET /works?filter=doi:A|B|…` (up to 100 per call) maps library DOIs to OpenAlex works. Each work has `referenced_works`, the references out.
- `GET /works?filter=cites:W1|W2|…&sort=cited_by_count:desc` lists citing works, the citations in.
- `GET /works?filter=openalex:W1|…` fetches metadata for works outside the library.
- Cost: a single work is free, and each list page costs $0.0001. Without a key, $0.10 a day is shared per IP; a free key gives $1 a day. A whole-library graph of about 430 references takes roughly 20–40 pages.
- The optional API key goes in the secret store as `openalex`, like the LLM keys, never in a file.
- References without a DOI are left out, and the view says how many. arXiv papers resolve through their `10.48550/arXiv.*` DOI.

## Components
- **yeda-net `citations`:** `works_by_doi`, `works_by_id`, and `citing(ids, limit)`. It sends HTTP and parses JSON into a `Work { id, doi, title, year, authors, venue, referenced_works, cited_by_count }`. It doesn't touch the disk.
- **yeda-core `graph`** (pure):
  - `build(lib, seeds, works) -> Graph`: nodes are library references matched by DOI, plus outside works; edges run from citing to cited.
  - Rankings, as in Local Citation Network:
    - **Top cited (▲):** outside works cited by the most seeds.
    - **Top citing (▼):** outside works citing the most seeds, counted from each citing work's `referenced_works`.
  - `layout(&Graph) -> positions`: one row per year, newest at the top, ordered within a row to reduce crossings. It lives here so that Android reuses it.
  - Serialization to and from the cache format.
- **Cache (client side):** `~/.cache/yeda/openalex/<work id>.json`, plus a DOI→id map; Android uses its cache directory. It is not stored in the library, since this is derived data that changes and would add git noise. `:graph!` re-fetches.

## yeda-tui
- `:graph` uses the visible references as seeds: the current collection, tag or search. `G` uses the selected reference and its neighbours.
- **Graph tab:** a ratatui `Canvas` (braille), which ratatui already includes, so there's no new dependency.
  - Library papers are `●` and outside papers `○`/`△`/`▽`; the selected node's edges are highlighted.
  - The year axis is on the left. `hjkl` moves between nodes, `+`/`-` zoom, and `Tab` cycles the tabs.
- **Lists tab:** Seeds, Top cited (▲) and Top citing (▼) as tables. Each row shows the count of seeds, year, first author and title.
- On a node or row:
  - `Enter` jumps to the reference if it's in the library.
  - `a` adds an outside paper through the magic wand by its DOI, so the batch-failure handling applies.
  - `o` opens the DOI in the browser.
  - The details pane shows the selected work's title, authors, venue, and in/out lists.
- Fetching runs on the existing worker thread with progress in the status line. API errors and budget exhaustion (429) are reported, not retried in a loop.

## yeda-android (second step)
- The same commands, with a Graph screen drawing the core layout as SVG (tap a node to see details, pinch to zoom) and the same list tabs. This keeps the two apps equal in features.

## Tests
- core: building a graph and ranking from a few fixed `Work`s; the layout puts newer years above older ones.
- net: parse a saved OpenAlex response; a live test is `#[ignore]`d.
- TUI: run in tmux on a copy of the library.

## Open questions
1. Seeds: are the visible references (`:graph`) and the selected one (`G`) the right defaults?
2. How many outside papers should be shown by default? (Local Citation Network uses top 10/20/50.)
3. Should the OpenAlex id be written into each reference's TOML once matched? It saves lookups but adds a field to every file.
4. Semantic Scholar as a second source: later, or not at all?
