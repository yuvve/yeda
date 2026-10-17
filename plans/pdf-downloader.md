# PDF downloader

Fetch the PDF for references that have none, then attach it the way `:attach` does. Sources: open-access copies (arXiv and others), and publishers through LTU's library proxy. EBSCO links lead to the publishers too.

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
`proxy.lib.ltu.se` is EZproxy, proxying by host name: `research.ebsco.com` becomes `research-ebsco-com.proxy.lib.ltu.se`.
- Signing in goes through LTU's CAS (`weblogon.ltu.se`). It asks again each time the EZproxy session expires.
- Any URL opens through it as `https://proxy.lib.ltu.se/login?url=<url>`, so a paper is `…?url=https://doi.org/<doi>`.
- On campus, EZproxy usually recognizes the network and skips the sign-in, so the same link works at work and at home.

```toml
[fulltext]
proxy = "https://proxy.lib.ltu.se/login?url={url}"
```

## How each source is covered
| Source | Option | How |
|---|---|---|
| arXiv and other open-access copies (OpenAlex `locations[].pdf_url`) | 1 | Plain HTTP; arXiv at about one request every 3 s. |
| Paywalled publishers: IEEE, Elsevier, Springer, AIAA, ACM, Wiley, … | 3, mostly automatic | The bundled browser, through the proxy. |
| The same, while a proxy session is open, for publishers with a known PDF URL (Springer `content/pdf/<doi>.pdf`, IEEE `stampPDF/getPDF.jsp?arnumber=…`) | 2 | Plain HTTP with the bundled browser's session cookie, no window. |
| EBSCOhost-hosted full text, sites with bot checks, pages without a PDF link | 3 | The bundled browser stays open; you click through, and the download is caught. |
| No DOI (books, mostly) | 5 | `:attach`, as now. |

Elsevier needs no API key or token: ScienceDirect works through the proxy in the bundled browser, as it does in your browser.

## The bundled browser
A yeda window with the system web engine yeda already ships for the reader: WebKitGTK through Tauri on desktop, Android's WebView on the phone. It is not a modified Firefox or Chromium, which would mean shipping a browser of over 100 MB.

- **Signing in:** when the proxy redirects to `weblogon.ltu.se`, the window shows the CAS page and waits. Once you sign in, the queue continues by itself. EZproxy's session cookie stays in the window's own profile in yeda's app data, never in the library. When the session expires, the next paper lands on the CAS page again and the same thing happens.
- **Fetching a paper:** yeda opens its DOI through the proxy, then follows the landing page's `citation_pdf_url` meta tag, which most publishers include for Google Scholar.
- **Downloads:** every download in the window goes to yeda's own downloads folder (default `<data dir>/yeda/downloads`, set by `[fulltext] downloads`). Tauri's `on_download` sets the destination, and on Android the WebView's download listener does. Your regular Downloads folder is never watched.
  - A PDF is attached to the paper being fetched, after checking the DOI or title on its first page, and moved to the chosen location.
  - A file that matches no paper stays in the folder and is listed.
- **When automation stops:** a bot check, a page without the meta tag, or an EBSCO viewer. The window stays on that page for you to click through, and the download is still caught. yeda doesn't disguise itself or solve checks, so the publishers' rules aren't broken.
- **Without a window:** while the session is open, papers whose PDF URL follows a known pattern are downloaded with plain HTTP, using the window's cookie (Tauri `cookies_for_url`). If one lands on the CAS page, it goes back to the window.
- **Pace:** one paper at a time with a pause between, and a cap per run. Publishers have cut off whole universities over bulk downloading.
- On desktop it runs as `yeda fetch <key>…`, a window from the yeda-reader crate like `yeda read`. On Android it's a screen in the app.

## Design
- **yeda-core `fulltext`** (pure):
  - builds a reference's candidate URLs: open-access PDFs from OpenAlex, then the proxied DOI, plus known publisher PDF URLs (DOI prefix → URL template; `yeda.toml` can add more);
  - rewrites URLs through the proxy;
  - matches a PDF's first-page text to a reference by DOI, else by title.
- **yeda-net `fulltext::download(url, cookie)`:** the response counts only if it starts with `%PDF-`. A login or challenge page means "needs the window".
- **Clients:**
  - `:fetch` (TUI) and "Find PDF" (Android) act on the selection or the marked references. They try option 1 first, then the window for the rest.
  - They report the same way as the batch magic wand: what was attached, and what failed with the reason, with the failures copied to the clipboard.
  - A fetched PDF goes through the same code as `:attach`: it's copied into a local `[locations]` folder, or uploaded to a WebDAV one in Zotero's layout, with a copy in the cache. The file in the downloads folder is removed only after that succeeds. The location is `[fulltext] location`, and `:fetch <location>` overrides it; with neither set, yeda asks.

## Phases
1. Open access (option 1), in TUI and Android, with the failure report.
2. The bundled browser: CAS sign-in, DOI → `citation_pdf_url` → download into yeda's folder → attach.
3. Reusing its session for plain-HTTP downloads of known publisher PDF URLs (option 2).
