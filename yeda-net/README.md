# yeda-net

Part of [yeda](https://github.com/yuvve/yeda), and not meant to be used on its own: install yeda with `cargo install yeda`.

The networking every yeda client shares: metadata lookup (doi.org, OpenLibrary, OpenAlex), LLM requests (Anthropic, Mistral, OpenRouter and OpenAI-compatible servers) and PDF transfers to WebDAV and Dropbox.

It never touches the disk: PDFs and API keys come in as arguments. Rejected credentials are reported as `remote::Unauthorized`, so the client can ask again.
