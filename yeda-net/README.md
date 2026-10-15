# yeda-net

The networking every yeda client shares. It never touches the disk: PDFs and API keys come in as arguments.

- `wand::fetch(input)` takes a DOI, arXiv ID or ISBN and returns a filled-in `Ref`. DOIs and arXiv IDs are resolved through doi.org content negotiation (CSL-JSON), and ISBNs through OpenLibrary.
- `llm::complete(profile, prompt, key)` sends a prompt to Anthropic or to any OpenAI-compatible server and returns the reply text.
  - Anthropic receives the PDF itself, up to 32 MB.
  - OpenAI-compatible servers receive the PDF's text, from the caller-supplied `Paper::text` callback.
