# yeda-tui

<!-- embed -->
**tl;dr:** a (vibe-coded) terminal reference manager whose library you can check into git, with a PDF reader built on Zotero's (so Zotero annotations carry over), citation graphs (including discovery of papers not in the library), a download assistant that makes fetching PDFs easy, LLM features such as paper discovery and summary tables, and an e-ink friendly companion Android app.

**🤖💩 All of yeda's code and documentation is written by AI (Claude). This is 100% vibe-coded 💩🤖**

I used [Zotero](https://www.zotero.org/), but wanted a reference manager in the terminal with vim keys. I tried every terminal reference manager I could find and didn't like any of them. I also really like [Local Citation Network](https://localcitationnetwork.github.io/), and wanted features like it built into my reference manager. So ~~I~~ Claude made yeda.

## About

yeda is a reference manager for the terminal, and an alternative to Zotero. Your library is a folder of plain TOML and Markdown files that works with git and Syncthing; PDFs live in a local or WebDAV folder.

From one terminal window, with vim keys, you can:
- add references by DOI, arXiv ID or ISBN, type them in, or import your whole Zotero library;
- organize them with collections, tags, reading status, search and saved searches;
- read and annotate PDFs in the built-in reader, a modified version of Zotero's PDF reader that supports Zotero annotations, and keep Markdown notes;
- find PDFs, open access or through your library's proxy;
- explore citation graphs of what a paper cites and what cites it;
- compare papers in literature tables an LLM fills in, and find related papers;
- cite as BibTeX or in a citation style.

An Android app reads and annotates the same library.

## Dependencies

yeda runs on Linux only; macOS and Windows have no secret store backend yet ([#292](https://github.com/yuvve/yeda/issues/292)).

To build it:
- Rust;
- the system libraries of [Tauri's prerequisites](https://v2.tauri.app/start/prerequisites/).

To run it:
- WebKitGTK 4.1, for the PDF reader;
- poppler (`pdftotext`, `pdftoppm`), for full-text search, annotation extraction and summaries;
- a running Secret Service provider (GNOME Keyring, KWallet or KeePassXC) to save passwords and API keys;
- git, for syncing the library;
- glibc 2.34 or later, for the release binary.

## Usage

```sh
cargo install yeda
```

Or download the binary from the [latest release](https://github.com/yuvve/yeda/releases/latest). The binary is `yeda`.

```sh
yeda init       # once: create a library
yeda import     # or instead of init: create it from Zotero
yeda            # open it
```

`yeda <library dir>` opens a specific library. Without one, yeda opens the current folder if it is a library, else the default library.

> **yeda has a built-in key list and a full guide.**

## Getting started

`yeda init` creates an empty library. A form asks for its folder, where its PDFs go (a local folder, a WebDAV folder, or none yet), and whether it becomes the default library. The library is ready for git and Syncthing.

`yeda import` creates a library from Zotero's instead. It imports the items, annotations, notes, tag colours and saved searches, and copies every PDF to the chosen place, whether Zotero stored it, linked it, or only has it on its WebDAV server. Zotero can stay open; it is only read.

## Main window

![Main window](screenshots/main.png)

Three columns: the library (collections, tags and saved searches), the list of references, and a details column that can also show annotations, a literature table or a citation graph. References show their reading status (new, to read, in progress, read) and a dot per coloured tag. Several references can be marked to act on together. A narrow terminal hides the details column, then the library. Alternate rows are shaded a little lighter than the terminal's background, on terminals that report it.

## Reading and annotating

References open in yeda's own PDF reader, controlled with vim keys. It highlights, underlines, and adds notes, text, image areas and ink. Annotations are saved in the library next to the paper, so the Android app shows them too. The reader has fuzzy-searchable lists of the PDF's sections, its annotations and its own keys.

Column mode reads a two-column paper one column at a time, each as wide as the window, a screen per page turn. The columns' position is set by dragging a crop box and the gutter over the page, and a new paper starts with the last ones set.

![Reader](screenshots/reader.png)

## Settings

`~/.config/yeda/config.toml` holds this device's settings; yeda creates it with them commented out:

```toml
library = "~/yeda"               # the default library
reader = "default"               # yeda's own PDF reader

[locations]                      # this device's folder for a location
pdfs = "/mnt/papers"
```

`reader` can name another command, run as `<command> <pdf> --annotations <dir>`. yeda then sees only the annotations that reader saves in the PDF.

LLM providers and profiles, WebDAV locations, the OpenAlex key, web search, library proxies and tables are set up in forms inside yeda and stored in the library's `yeda.toml`.

## Graphs

A citation graph of a paper, or of several marked ones, with years as rows and the newest at the top:

![Citation graph](screenshots/graph.png)

- ⊚ the papers you graphed, ◎ other library papers linked to them, ○ papers outside the library cited by or citing them most
- yellow lines go to papers the selected one cites, magenta lines come from papers citing it

A paper in the graph can be opened to grow the graph from it, or added to the library. Papers can also be grouped by their tags or by a literature table column. Data comes from OpenAlex and is cached.

## LLM features

Both features below use an LLM profile from the library's `yeda.toml`. A profile names a provider and a model; the provider holds where to connect and how:

```toml
default_llm = "claude"

[providers.claude]
base_url = "https://api.anthropic.com"
protocol = "anthropic"            # or "openai", "mistral", "openrouter", "opencode-go", "openai-responses"
auth = "oauth"                    # or "api-key", or "none" for a server without a key

[llm.claude]
provider = "claude"
model = "claude-opus-5-5"
```

Profiles bound to one provider share its credential. A command uses `default_llm` unless it names another profile.

### Literature tables

A literature table compares papers side by side: one row per paper, one column per question you ask of every paper. Each column has an optional description of what it should hold; in `yeda.toml` they look like this:

```toml
[tables.lit]
columns = ["contribution", "group", "subproblem"]

[tables.lit.describe]
contribution = "What the paper adds, in plain words; no jargon."
group = "The topic it works on: a short label many papers share."
subproblem = "The problem it tackles and why earlier work falls short."
```

The LLM reads each selected paper and fills in its row. It also sees the values other papers already have in each column and reuses one when it fits, so a column like `group` sorts papers into shared topics.

Cells can be read in full, edited, sorted and filtered. A table exports to CSV and imports back, and the graph can cluster papers by a column's values.

![Literature table](screenshots/table.png)

### Finding papers

Discovery finds papers that are not in the library. Given some seed papers (the marked ones), a description of what to look for and an LLM, yeda searches the web and arXiv, and the model ranks what it finds. Results can be added to the library. The search providers are configurable.

```toml
[web_search]
always_search_providers   = ["keenable"]
fallback_search_providers = ["exa", "parallel", "firecrawl"]
disabled_search_providers = ["model_provider_web_search", "searxng"]
discover_queries          = 3        # queries the model writes per :discover
discover_results          = 15       # papers shown
# searxng_url             = "https://searx.example.org"
```

Fallbacks run only if every "always" provider fails. Without an API key a provider uses its shared free tier; keyed and keyless calls may use different endpoints. Results are cached in `$XDG_CACHE_HOME/yeda`. `yeda.toml` is trusted like code, so a changed `searxng_url` sends queries elsewhere.

## Secrets

Passwords and API keys are stored in the system secret store (GNOME Keyring, KWallet or KeePassXC); yeda asks for a missing one and offers to save it. LLM profiles bound to the same provider share one API key or OAuth credentials. Web search provider keys are optional and only used by discovery. Keys are stored with their server's address, so a key is only sent to the server it was entered for. Passwords, API keys and tokens only go to `https://` servers, local ones included: LLM servers, WebDAV locations, web search providers and library proxies with a plain `http://` address are refused.

Environment variables can serve as a fallback or an alternative to the secret store, and override it.

## Keys and commands

![The ? key list](screenshots/keys.png)

Every action is a command, bound to keys that can be changed in `config.toml`. A built-in key list and guide are available inside yeda.

## Scripting

Most commands also run without the TUI, for scripts and agents. Each subcommand takes references by key (or `-` to read keys from stdin), and prints keys or tab-separated lines (`--json` gives more):

```sh
yeda search tag:review --sort -year               # key, year, authors, title
yeda search coll:thesis | yeda cite - --format apa
yeda add 10.1038/nature14539 --to thesis
yeda graph lecun2015 > graph.json
yeda discover "swarm exploration under comms loss" --json   # queries and notes go to stderr
```

They use the library's settings and the secrets saved from the TUI.
<!-- /embed -->
