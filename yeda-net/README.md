# yeda-net

The networking every yeda client shares: metadata lookup (doi.org, OpenLibrary, OpenAlex), LLM requests (Anthropic and OpenAI-compatible servers) and WebDAV transfers.

It never touches the disk: PDFs and API keys come in as arguments. A rejected WebDAV password is reported as `webdav::Unauthorized`, so the client can ask again.
