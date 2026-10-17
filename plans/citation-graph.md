# Citation graph

A view of how papers cite each other: references out of a paper, citations into it, and how papers connect, both inside the library and outside it. It is modelled on Local Citation Network.

## Decisions
- It's a command only: `:graph [selected|filtered|prefetch]`, where `selected` (the default) means the marked references, else the one under the cursor. `G` stays vim's.
- It shows 20 papers outside the library by default (`[graph] outside`).
- A matched reference stores its OpenAlex id as `openalex = "W…"`.
- The graph stays open in the right column. It highlights the paper selected in the list and can be navigated, so it doubles as a way to move through the library.
- References out are always fetched in full, since they are part of each work. Citations in are limited to the most cited citing papers per seed (`[graph] citing`, default 100, one request per seed). Citations from inside the library are always complete, because they come from the library papers' own reference lists.
- An OpenAlex API key is optional: `:graph key`, `OPENALEX_API_KEY`, or a prompt when the keyless budget runs out. It's kept in the secret store.
- Semantic Scholar can be added later. `graph::Work` doesn't depend on the source, and a second client would sit next to `yeda_net::openalex`.

## Built (yeda-tui)
- **yeda-net `openalex`:**
  - works by DOI (batched; a free single lookup when OpenAlex reports the work under another DOI, as with an arXiv preprint published later), by id, and citing works sorted by citation count;
  - errors for a used-up budget (429) and a rejected key.
- **yeda-core `graph`:** `Work`, `candidates` (which outside works the ranking needs metadata for), `build` (seeds, the library papers linked to them, and the outside papers linked to the most seeds), and a layout with one row per year.
- **yeda-tui:**
  - `citations.rs` fetches on a thread and caches each work and each citing list under `~/.cache/yeda/openalex/`;
  - `:graph!` fetches the citing papers again;
  - in the graph pane: j/k pick a neighbour, Enter goes there, Backspace goes back, `a` adds the paper by DOI, `o` opens its DOI.

## Next
- yeda-android: the same commands, with a graph screen drawing the core layout as SVG.
- Title search for the references without a DOI (books, mostly).
- Lists of "top cited" and "top citing" papers, as in Local Citation Network.
- Semantic Scholar.
