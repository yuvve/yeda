# yeda-secrets

Keeps secrets (WebDAV passwords, LLM API keys) in the system's secret store, never in a file:

- **Linux:** the Secret Service, as served by GNOME Keyring, KWallet or KeePassXC. yeda only uses a provider that is already running; it never lets D-Bus start one, so no keyring prompt appears unasked.
- **Android:** the Keystore.

Entries have service `yeda`, and an account of `webdav:<user>@<url>`, `llm:<ENV_VAR>`, `git:<remote url>` or `api:openalex`.

For KeePassXC, enable *Settings → Secret Service Integration*. Then, in *Database Settings → Secret Service Integration*, expose the group that should hold yeda's entries.
