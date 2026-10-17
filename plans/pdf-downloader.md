# PDF downloader

Fetch the PDF for references that have none, then attach it the way `:attach` does. Sources: open-access copies (arXiv and others), and publishers through institutional access — the campus network (IP), or a library proxy at home. EBSCO links lead there too.

## What the library needs
Of 430 references, 61 have no PDF:
- arXiv (`10.48550`): 25
- IEEE (`10.1109`): 10
- AIAA (`10.2514`): 3
- Elsevier (`10.1016`): 3
- 1–2 each from other publishers: 11
- no DOI: 9

In a sample of 40 library DOIs, OpenAlex listed an open-access PDF for 29: 22 of them on arXiv, 7 elsewhere. 9 were closed. (Fetching a single work from OpenAlex is free.)

## The five options, by source
| Source | Best option | Why |
|---|---|---|
| arXiv and other open-access copies (from OpenAlex's `locations[].pdf_url`) | 1, fully automatic | Plain HTTP. arXiv allows scripted downloads at about one request every 3 s. |
| Publishers that serve a PDF at a fixed URL (Springer `link.springer.com/content/pdf/<doi>.pdf`, IEEE `stampPDF/getPDF.jsp?arnumber=…`) | 1 on campus; 2 at home with an EZproxy session cookie | IP authentication needs nothing from you. EZproxy works by prefixing the URL, and its session cookie can be reused. To be verified per publisher. |
| Elsevier | 1 on campus, through Elsevier's Article Retrieval API with a free API key | At home it needs an institution token from the library, otherwise option 4. |
| Sites behind Cloudflare or other bot checks (often ACM, Wiley, some IEEE pages) | 4 | yeda won't try to get around bot checks: they are there to stop scripted downloads, and publishers have blocked whole universities over bulk downloading. |
| EBSCOhost-hosted full text | 4 | Its viewer needs a browser session. The EDS API needs credentials that the library holds, not you. In this library, EBSCO results mostly lead to the publishers above anyway. |
| OpenAthens instead of EZproxy | 4 (3 at best) | Login goes through each publisher separately (SAML), so there's no single cookie to reuse. |
| No DOI (books, mostly) | 4 or 5 | |

So option 1 covers about 30 of the 61. Option 2 adds some of the IEEE, Springer and similar papers if the home proxy is EZproxy, and option 4 catches the rest.

## Design
- **yeda-core `fulltext`** (pure):
  - builds candidate URLs for a reference from its DOI, the OpenAlex locations, `[fulltext]` settings and a small table of publisher patterns (DOI prefix → PDF URL template, which `yeda.toml` can extend);
  - rewrites URLs through the proxy, e.g. `proxy = "https://ezproxy.example.edu/login?url={url}"`, which is tried after the direct URL;
  - matches a downloaded PDF to a reference by the DOI on its first page, else by title.
- **yeda-net `fulltext::download(url, cookie)`:**
  - it identifies itself honestly as yeda, makes one request at a time with a pause between, and caps paywalled downloads per run;
  - the response counts only if it starts with `%PDF-`;
  - a challenge or login page is reported as "needs the browser", never retried or worked around.
- **Clients:**
  - `:fetch` (TUI) and "Find PDF" (Android) act on the selection or the marked references.
  - They report the same way as the batch magic wand: what was attached, and what failed with the reason, with the failures copied to the clipboard.
  - PDFs are saved to a chosen `[locations]` entry, local or WebDAV, as `:attach` does.
- **Option 4, the inbox:**
  - for the references that failed, `:fetch` can open each one's DOI (through the proxy) in the browser;
  - yeda then watches a dedicated folder (`[fulltext] inbox`, for example `~/Downloads/yeda`, or Download on Android) and attaches each new PDF to the reference it matches;
  - a PDF that matches nothing stays in the folder and is listed.
- **Option 2, the cookie:** the EZproxy session cookie is pasted into a masked prompt (`:fetch cookie`) and kept in the secret store, never in a file. It is a login credential. Reading it from Firefox's cookie database would be an opt-in alternative.
- **Option 5** already exists: `:attach` and Android's Attach PDF.

## Phases
1. Open access (option 1), in TUI and Android, with the failure report.
2. The inbox and opening failed references in the browser (option 4).
3. Institutional access by IP and through the proxy with a cookie (options 1 and 2), publisher by publisher, after testing each on campus and at home.
4. The Elsevier API, with an optional key in the secret store.

## Open questions
1. At home, is the proxy EZproxy or OpenAthens? What does a proxied link look like?
2. Which browser do you use, and should the inbox be a dedicated folder or your Downloads folder?
3. For option 2: paste the cookie, or let yeda read it from your browser (opt-in)?
4. Does your library give out an Elsevier institution token? Only needed for Elsevier from home.
