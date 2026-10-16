# TODO

## Features
- **Citation graph in yeda-tui.** Show a paper's references (out) and the papers citing it (in), and how papers connect, both in the library and outside it. Rendered in the terminal if possible; a separate view is acceptable. Citation data comes from an external API (for example OpenAlex, Semantic Scholar or OpenCitations), fetched through yeda-net. Similar to Local Citation Network (https://localcitationnetwork.github.io). Plan: `plans/citation-graph.md`.
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
- Android on the phone: the batch add, the filter suggestions, git sync and clone.
- KeePassXC as the Secret Service provider.
- ViWoods AiPaper: pen and e-ink.
- LLM tables on Android (need an API key).
