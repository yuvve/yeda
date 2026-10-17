# TODO

## Features
- **Smaller yeda-android:** keep reading, annotating, sorting, filtering, searches, tags and collections; drop what desktop covers, such as the magic wand, citation graph and LLM tables.
- **Citation graph in yeda-android** (yeda-tui has `:graph`). Also: Semantic Scholar as a second source, and title search for references without a DOI.
- **Pen and e-ink (yeda-android):**
  - route input by `pointerType`: the pen uses the active tool, a finger pans and zooms, and touch is ignored while the pen is down;
  - erase while the EMR side button (`buttons & 2`) or eraser end (`buttons & 32`) is held;
  - an e-ink mode: no animations, page jumps instead of scrolling, high contrast, larger touch targets;
  - log the pressure values the WebView reports.
- **Low-latency ink (stretch):** a Tauri plugin using androidx.graphics front-buffered rendering; keep it only if the measured lag improves.
- **Remote storage** beyond WebDAV, through `opendal`.
- **Zotero exporter:** push references and annotations to Zotero through its web API.
- **Zotero attachment items:** let yeda create them.
- **Help screen** in yeda-android.

## Known gaps
- WebDAV: a cached PDF isn't downloaded again when the copy on the server changes.
- Reader: the page-number label (`/ 15`) slightly overlaps the next toolbar icon.
- Android: `window.prompt` ignores its default value, so the tag-colour prompt isn't prefilled.
- Clippy: `Searcher` lacks `Default`.

## To test
- PDF fetching through LTU's proxy (needs your sign-in), on desktop and on the phone.
- Android on the phone: the batch add, the filter suggestions, git sync and clone.
- KeePassXC as the Secret Service provider.
- ViWoods AiPaper: pen and e-ink.
- LLM tables on Android (need an API key).
