# yeda-net

The networking every yeda client shares. It never touches the disk: PDFs and API keys come in as arguments.

- `wand::fetch(input)` takes a DOI, arXiv ID or ISBN and returns a filled-in `Ref`. DOIs and arXiv IDs are resolved through doi.org content negotiation (CSL-JSON), and ISBNs through OpenLibrary.
- `llm::complete(profile, prompt, key)` sends a prompt to Anthropic or to any OpenAI-compatible server and returns the reply text.
  - Anthropic receives the PDF itself, up to 32 MB.
  - OpenAI-compatible servers receive the PDF's text, from the caller-supplied `Paper::text` callback.
- `webdav::download`, `webdav::upload` and `webdav::current_version` move a PDF to and from a WebDAV server as a plain file, creating its folder, and report its ETag so a cached copy is downloaded only when it changed. A rejected password is reported as `webdav::Unauthorized`, so the client can ask again.
- `openalex::Client` looks up works, citing works and open-access PDF URLs.
- `fulltext::download(url)` downloads an open-access copy and accepts it only if it is a PDF.
