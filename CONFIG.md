# Configuration

## Opening a library

`yeda <library dir>` opens a specific library. Without one, yeda opens the current folder if it is a library, else the default library.

`yeda import` takes Zotero's items, annotations, notes, tag colours and saved searches, and copies every PDF to the chosen place. Zotero can stay open; it is only read.

## The library

```
<library>/
  yeda.toml                               # settings shared by every device
  refs/<citekey>.toml                     # one reference each
  notes/<citekey>.md                      # your notes
  notes/<citekey>.ai.toml                 # what an LLM wrote for the tables
  annotations/<citekey>/<pdf>/annotations.toml  # annotations on one PDF
  annotations/<citekey>/<pdf>/img/<id>.png      # image and ink annotations
```

PDFs are kept outside the library folder, in *locations*: a local folder, a WebDAV folder or Dropbox, each holding `<citekey>/<file>`. Remote PDFs are cached locally. `:setup storage` adds a location; `:help` has the steps for using your own Dropbox app. A reference can have several PDFs, each a `[[pdf]]` entry whose `id` names its annotations folder.

### Shared settings

`yeda.toml` holds what every device shares, such as the locations and saved searches:

```toml
[locations]
pdfs = { kind = "local", path = "~/yeda-pdfs" }
dav = { kind = "webdav", url = "https://dav.example.com/yeda/", user = "me" }
box = { kind = "dropbox", folder = "pdfs" }

[searches]
"To read" = { query = 'status:"^To Read$"', sort = "year", descending = true }
```

Each device also has its own config, whose `[locations]` may give a location a different folder on that device: `~/.config/yeda/config.toml` on the desktop, the app's settings on Android.

## LLMs

LLM features use a profile from `yeda.toml`: a provider and a model. `:setup provider` and `:setup llm` add them.

A provider is a preset (`anthropic`, `codex`, `opencode-go`, `openrouter`) or set by hand: a `protocol` (`chat-completions`, `responses` or `messages`), a `base_url` and an `auth` (`api-key` or `none`). `codex` signs in with a ChatGPT plan. `:help providers` and [PROVIDERS.md](yeda-core/PROVIDERS.md) show every preset as config; to change one, copy its table under a name of your own and edit it. If a preset breaks, fix your copy and send the working table as a pull request. A provider set by hand can add `headers`, which take `{app}`, `{version}`, `{repository}`, `{install}`, `{session}` (one per command) and `{request}`.

A profile can set `documents` (`file`, or `text` for a model that takes no PDF), `structured_output` (`json-schema`, `json-object` or `prompt`), `reasoning_effort`, `max_output_tokens` and `body`. `:setup llm` tests these by sending a one-page PDF and a schema, and says which option to change when the model cannot read it. Each command ends with the tokens it used, and the cost when the provider reports it.

```toml
default_llm = "claude"

[providers.claude]
preset = "anthropic"

[providers.lan]
protocol = "chat-completions"
base_url = "https://llm.lan/v1"
auth = "none"

[llm.claude]
provider = "claude"
model = "claude-opus-5-5"

[llm.local]
provider = "lan"
model = "qwen3"
documents = "text"
```

A plan can only use `:add`, `:fetch`, `:summarize`, `:discover`, `:search`, `:cache`, `:extract`, `:cite`, `:table`, `:graph`, `:export`, `:save` and `:sync`. The request, the names of collections, tags, tables and locations, and the titles of the marked (else selected) references go to the LLM server.

Search providers for `:discover` are set with `:setup web-search`.

## Secrets

Passwords and API keys live in the system secret store (`:secrets`), each tied to its server's address, so a key is only sent to the server it was entered for. Secrets only go to `https://` servers. Environment variables can replace or override the secret store (`:envvars`). A SearXNG instance behind basic auth takes its user name and password through `:setup web-search searxng`, in the secret store, never in the URL.

## Trust

`yeda.toml` is trusted like code: whoever can write to the library's git remote or Syncthing folder decides which servers get LLM requests, which proxies the fetch window uses and which WebDAV servers hold the PDFs. A Dropbox location can only point at Dropbox. They cannot get a secret: each is stored with its server's address, so a changed address finds none and yeda asks, naming the server.

PDF downloads go only over `https://` to public hosts (no `localhost`, `.local`, private or link-local addresses), through at most 5 redirects, and stop at 100 MB.
