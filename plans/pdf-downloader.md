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

## The library proxy
`proxy.lib.ltu.se` is EZproxy, proxying by host name: `research.ebsco.com` becomes `research-ebsco-com.proxy.lib.ltu.se`. Signing in goes through LTU's CAS (`weblogon.ltu.se`). Any URL can be opened through it as `https://proxy.lib.ltu.se/login?url=<url>`, so a paper is `…?url=https://doi.org/<doi>`. On campus, EZproxy usually recognizes the network and skips the sign-in, so the same link works at work and at home. This goes in the shared config:

```toml
[fulltext]
proxy = "https://proxy.lib.ltu.se/login?url={url}"
```

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

## A bundled browser
This is the best way to cover the paywalled papers. It's not a modified Firefox or Chromium, which would mean shipping a browser of over 100 MB. It's a yeda window with the system web engine that yeda already uses for the reader: WebKitGTK through Tauri on desktop, and Android's WebView.

- **You sign in once.** The CAS page opens in the window and you sign in as usual. EZproxy's session cookie then stays in the window's own profile, in yeda's app data, not in the library.
- **yeda opens each paper.** It goes to the paper's DOI through the proxy, then follows the landing page's `citation_pdf_url` meta tag, which most publishers include for Google Scholar.
- **Downloads are attached.** yeda catches every download in the window (Tauri `on_download`; on Android, the WebView's download listener) and attaches the PDF to the paper being fetched.
- **Bot checks and odd pages:** the window is a normal visible browser, so you solve the check or click the PDF link yourself, and the download is still caught. yeda doesn't hide what it is or solve checks for you, so the publishers' rules aren't broken. This makes option 3 close to automatic for most sites.
- **Reusing the session:** with the session cookie (Tauri `cookies_for_url`), the papers with known PDF URLs (the publisher table above) can be downloaded with plain HTTP and no window, until the session expires. That is option 2 without copying cookies by hand.
- On desktop it runs as `yeda fetch <key>…`, a window from the yeda-reader crate like `yeda read`. On Android it's a screen in the app.

Controlling your own Firefox (through WebDriver) would reuse your existing sign-in, but sites see it as automated (`navigator.webdriver`) and are more likely to block it, so the embedded window is the better choice.

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
- **Option 2, the cookie:** comes from the bundled browser's session (see above), so there's nothing to paste.
- **Option 5** already exists: `:attach` and Android's Attach PDF.

## Phases
1. Open access (option 1), in TUI and Android, with the failure report.
2. The bundled browser: sign in to the proxy, go DOI → `citation_pdf_url` → download → attach.
3. Reusing its session for plain-HTTP downloads of known publisher PDF URLs (option 2).
4. The inbox for PDFs downloaded in your own browser (option 4), and the Elsevier API.

## Open questions
1. Does LTU's CAS sign-in remember you, or ask every time the EZproxy session expires (usually after a couple of hours of inactivity)?
2. For the inbox (phase 4): a dedicated folder, or your Downloads folder?
3. Does your library give out an Elsevier institution token? Only needed for Elsevier from home.
