//! Secrets (WebDAV passwords, LLM API keys) in the system's secret store: the Secret Service on
//! Linux (GNOME Keyring, KWallet, KeePassXC) and the Keystore on Android. Never in a file.

use anyhow::{Context, Result};
use keyring_core::{Entry, Error};
use std::sync::Mutex;

const SERVICE: &str = "yeda";

fn entry(account: &str) -> Result<Entry> {
    // Retried until it works, so starting a provider mid-session is picked up.
    static READY: Mutex<bool> = Mutex::new(false);
    let mut ready = READY.lock().unwrap();
    if !*ready {
        keyring_core::set_default_store(store().context("opening the system secret store")?);
        *ready = true;
    }
    Entry::new(SERVICE, account).context("opening the system secret store")
}

#[cfg(target_os = "android")]
fn store() -> anyhow::Result<std::sync::Arc<keyring_core::CredentialStore>> {
    Ok(android_native_keyring_store::Store::new()?)
}

#[cfg(not(target_os = "android"))]
fn store() -> anyhow::Result<std::sync::Arc<keyring_core::CredentialStore>> {
    // Connecting would make D-Bus start the default provider (GNOME Keyring), whose first-use
    // prompt grabs the whole desktop. Only use a provider that is already running.
    let bus = zbus::blocking::Connection::session().context("connecting to the session bus")?;
    let name = zbus::names::BusName::try_from("org.freedesktop.secrets")?;
    if !zbus::blocking::fdo::DBusProxy::new(&bus)?.name_has_owner(name)? {
        anyhow::bail!("no secret store is running; enable the Secret Service integration in KeePassXC (or start another provider)");
    }
    Ok(zbus_secret_service_keyring_store::Store::new()?)
}

/// The environment variable that overrides `account`, named after the server's host so that a
/// shared config pointing a provider elsewhere finds no variable to send there. `None` for
/// accounts that have no server.
pub fn env_name(account: &str) -> Option<String> {
    let fixed = |host: &str| variable("KEY", &format!("https://{host}"));
    match account {
        OPENALEX => fixed("api.openalex.org"),
        KEENABLE => fixed("api.keenable.ai"),
        EXA => fixed("api.exa.ai"),
        PARALLEL => fixed("api.parallel.ai"),
        FIRECRAWL => fixed("api.firecrawl.dev"),
        _ => {
            let prefix = if account.starts_with("provider:") {
                "KEY"
            } else if account.starts_with("webdav:") {
                "DAV"
            } else {
                return None;
            };
            variable(prefix, &account[account.find("https://")?..])
        }
    }
}

/// `YEDA_<prefix>_<HOST>[_<PORT>]`; dots become `_` and hyphens `__`, which no host name contains.
fn variable(prefix: &str, url: &str) -> Option<String> {
    let authority = url.strip_prefix("https://")?.split(['/', '?', '#']).next()?;
    let (host, port) = match authority.split_once(':') {
        Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => (host, Some(port)),
        Some(_) => return None,
        None => (authority, None),
    };
    if host.is_empty() || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
        return None;
    }
    let host = host.to_ascii_uppercase().replace('.', "_").replace('-', "__");
    Some(match port {
        Some(port) => format!("YEDA_{prefix}_{host}_{port}"),
        None => format!("YEDA_{prefix}_{host}"),
    })
}

/// The value of the environment variable that overrides `account`, if it is set and not empty.
pub fn from_env(account: &str) -> Option<String> {
    let value = std::env::var(env_name(account)?).ok()?;
    Some(value.trim().to_string()).filter(|v| !v.is_empty())
}

/// The secret for `account`: its environment variable if set, else the one in the secret store.
pub fn get(account: &str) -> Result<Option<String>> {
    if let Some(secret) = from_env(account) {
        return Ok(Some(secret));
    }
    stored(account)
}

/// The secret in the secret store for `account`, ignoring the environment.
pub fn stored(account: &str) -> Result<Option<String>> {
    match entry(account)?.get_password() {
        Ok(s) => Ok(Some(s)),
        Err(Error::NoEntry) => Ok(None),
        Err(e) => Err(e).with_context(|| format!("reading {account} from the secret store")),
    }
}

pub fn set(account: &str, secret: &str) -> Result<()> {
    entry(account)?.set_password(secret).with_context(|| format!("saving {account} to the secret store"))
}

pub fn delete(account: &str) -> Result<()> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(Error::NoEntry) => Ok(()),
        Err(e) => Err(e).with_context(|| format!("removing {account} from the secret store")),
    }
}

/// The account name for a WebDAV login.
pub fn webdav_account(user: &str, url: &str) -> String {
    format!("webdav:{user}@{url}")
}

/// The account for the optional OpenAlex API key.
pub const OPENALEX: &str = "api:openalex";

/// The accounts for the optional web search provider keys, one per provider.
pub const KEENABLE: &str = "api:keenable";
pub const EXA: &str = "api:exa";
pub const PARALLEL: &str = "api:parallel";
pub const FIRECRAWL: &str = "api:firecrawl";

/// The account name for a git remote's password or access token.
pub fn git_account(url: &str) -> String {
    format!("git:{url}")
}

/// The account name for a provider's API key or OAuth credentials, shared by every profile bound to
/// it. It names the server too: a shared config that points the provider elsewhere finds no secret
/// to send there.
pub fn provider_account(name: &str, url: &str) -> String {
    format!("provider:{name}:{url}")
}

#[cfg(test)]
mod tests {
    #[test]
    fn env_names_follow_the_host() {
        let name = |a: &str| super::env_name(a);
        assert_eq!(name(&super::provider_account("claude", "https://api.anthropic.com/v1")).as_deref(), Some("YEDA_KEY_API_ANTHROPIC_COM"));
        assert_eq!(name(&super::provider_account("lan", "https://my-llm.lan:8443")).as_deref(), Some("YEDA_KEY_MY__LLM_LAN_8443"));
        assert_eq!(name(&super::webdav_account("me", "https://dav.example.com/yeda/")).as_deref(), Some("YEDA_DAV_DAV_EXAMPLE_COM"));
        assert_eq!(name(super::OPENALEX).as_deref(), Some("YEDA_KEY_API_OPENALEX_ORG"));
        assert_eq!(name(super::FIRECRAWL).as_deref(), Some("YEDA_KEY_API_FIRECRAWL_DEV"));
        assert_eq!(name(&super::git_account("https://github.com/a/b")), None);
        assert_eq!(name(&super::provider_account("x", "http://plain.example")), None);
        assert_eq!(name(&super::provider_account("x", "https://user@host.example")), None);
        assert_eq!(name(&super::provider_account("x", "https://[::1]:1")), None);
    }

    #[test]
    fn providers_on_one_host_share_a_variable() {
        let a = super::provider_account("a", "https://api.example.com/v1");
        let b = super::provider_account("b", "https://api.example.com/v2");
        assert_eq!(super::env_name(&a), super::env_name(&b));
    }

    /// Run as `dbus-run-session -- cargo test -p yeda-secrets -- --ignored`: a fresh bus has no provider running.
    #[test]
    #[ignore = "needs a session bus without a secret store"]
    fn never_starts_a_provider() {
        let e = super::get("test").unwrap_err();
        assert!(format!("{e:#}").contains("no secret store is running"), "{e:#}");
    }

    /// Run as `cargo test -p yeda-secrets -- --ignored round_trip` with the provider unlocked.
    #[test]
    #[ignore = "writes to the real secret store"]
    fn round_trip() {
        for account in [super::webdav_account("yeda-test", "https://example.invalid/dav"), super::provider_account("yeda-test", "https://example.invalid/v1")] {
            super::set(&account, "s3cret").unwrap();
            assert_eq!(super::get(&account).unwrap().as_deref(), Some("s3cret"));
            super::delete(&account).unwrap();
            assert_eq!(super::get(&account).unwrap(), None);
        }
    }
}
