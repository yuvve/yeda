//! Secrets (WebDAV passwords, Dropbox sign-ins, LLM API keys) in the system's secret store: the Secret Service on
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

/// The environment variables that override `account`, most specific first: the one for `alias`
/// (if any), then the one shared by every secret on the same server. The names follow the server's
/// host, so a shared config pointing a provider elsewhere finds no variable to send there. Empty
/// for accounts that have no server.
pub fn env_names(account: &str, alias: Option<&str>) -> Vec<String> {
    let (prefix, url) = match account {
        OPENALEX => ("KEY", "https://api.openalex.org"),
        KEENABLE => ("KEY", "https://api.keenable.ai"),
        EXA => ("KEY", "https://api.exa.ai"),
        PARALLEL => ("KEY", "https://api.parallel.ai"),
        FIRECRAWL => ("KEY", "https://api.firecrawl.dev"),
        _ if account.starts_with(DROPBOX_PREFIX) => {
            let Some(name) = env_name(&account[DROPBOX_PREFIX.len()..]) else { return Vec::new() };
            return vec![format!("YEDA_DROPBOX_{name}")];
        }
        _ => {
            let prefix = if account.starts_with("provider:") {
                "KEY"
            } else if account.starts_with("webdav:") {
                "DAV"
            } else if account.starts_with("searxng:") {
                "AUTH"
            } else {
                return Vec::new();
            };
            let Some(at) = account.find("https://") else { return Vec::new() };
            (prefix, &account[at..])
        }
    };
    let Some(host) = yeda_core::env_host(url) else { return Vec::new() };
    let base = format!("YEDA_{prefix}_{host}");
    let alias = alias.filter(|a| yeda_core::check_alias(a).is_ok());
    // A host whose last label is `alias` would read the variable of another host with an alias.
    let impersonates = base.rsplit_once("_ALIAS_").is_some_and(|(_, tail)| !tail.contains('_'));
    alias.map(|a| format!("{base}_ALIAS_{}", a.to_ascii_uppercase())).into_iter().chain([base].into_iter().filter(|_| !impersonates)).collect()
}

/// The first of `account`'s environment variables that is set and not empty, with its value.
pub fn env_hit(account: &str, alias: Option<&str>) -> Option<(String, String)> {
    env_names(account, alias).into_iter().find_map(|name| {
        let value = std::env::var(&name).ok()?.trim().to_string();
        (!value.is_empty()).then_some((name, value))
    })
}

/// The value of the environment variable that overrides `account`, if there is one.
pub fn from_env(account: &str, alias: Option<&str>) -> Option<String> {
    env_hit(account, alias).map(|(_, value)| value)
}

/// The secret for `account`: its environment variable if set, else the one in the secret store.
pub fn get(account: &str) -> Result<Option<String>> {
    get_as(account, None)
}

/// Like [`get`], for a secret whose provider or location has an `alias`.
pub fn get_as(account: &str, alias: Option<&str>) -> Result<Option<String>> {
    if let Some(secret) = from_env(account, alias) {
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

const DROPBOX_PREFIX: &str = "dropbox:";

/// The account of the location `name`: a WebDAV login, or a Dropbox sign-in. A Dropbox sign-in is
/// only ever sent to Dropbox's fixed servers, so the location's name is enough to find it.
pub fn remote_account(name: &str, remote: &yeda_core::library::Remote) -> String {
    use yeda_core::library::Remote;
    match remote {
        Remote::Webdav(d) => webdav_account(&d.user, &d.url),
        Remote::Dropbox(_) => format!("{DROPBOX_PREFIX}{name}"),
    }
}

/// A location name as part of an environment variable's name: letters and digits, `_`, and `-`
/// as `__`; `None` for a name that would not identify one location.
fn env_name(name: &str) -> Option<String> {
    let ok = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then(|| name.to_ascii_uppercase().replace('-', "__"))
}

/// The account for a SearXNG instance's `user:password`, which `:setup searxng` keeps out of the URL.
pub fn searxng_account(url: &str) -> String {
    format!("searxng:{url}")
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
        let names = |a: &str, alias| super::env_names(a, alias);
        let claude = super::provider_account("claude", "https://api.anthropic.com/v1");
        assert_eq!(names(&claude, None), ["YEDA_KEY_API_ANTHROPIC_COM_443"]);
        assert_eq!(names(&claude, Some("work")), ["YEDA_KEY_API_ANTHROPIC_COM_443_ALIAS_WORK", "YEDA_KEY_API_ANTHROPIC_COM_443"]);
        assert_eq!(names(&claude, Some("my-work")), ["YEDA_KEY_API_ANTHROPIC_COM_443"]);
        assert_eq!(names(&super::webdav_account("me", "https://dav.example.com/yeda/"), Some("a")), ["YEDA_DAV_DAV_EXAMPLE_COM_443_ALIAS_A", "YEDA_DAV_DAV_EXAMPLE_COM_443"]);
        assert_eq!(names(&super::searxng_account("https://searx.example.org/"), None), ["YEDA_AUTH_SEARX_EXAMPLE_ORG_443"]);
        assert_eq!(names(super::OPENALEX, None), ["YEDA_KEY_API_OPENALEX_ORG_443"]);
        assert_eq!(names(super::FIRECRAWL, None), ["YEDA_KEY_API_FIRECRAWL_DEV_443"]);
        assert_eq!(names("dropbox:my-box", None), ["YEDA_DROPBOX_MY__BOX"]);
        assert_eq!(names("dropbox:papers", Some("x")), ["YEDA_DROPBOX_PAPERS"]);
        assert!(names("dropbox:my.box", None).is_empty());
        assert!(names(&super::git_account("https://github.com/a/b"), None).is_empty());
        assert!(names(&super::provider_account("x", "http://plain.example"), None).is_empty());
    }

    #[test]
    fn an_aliased_name_cannot_be_another_hosts() {
        let account = |url| super::provider_account("p", url);
        // x.5.alias:443 would read the variable of host x, port 5, alias 443.
        assert!(super::env_names(&account("https://x.5.alias"), None).is_empty());
        assert_eq!(super::env_names(&account("https://x.5.alias"), Some("w")), ["YEDA_KEY_X_5_ALIAS_443_ALIAS_W"]);
        assert_eq!(super::env_names(&account("https://x.alias.work"), None), ["YEDA_KEY_X_ALIAS_WORK_443"]);
        assert_eq!(super::env_names(&account("https://x:5"), Some("443")), ["YEDA_KEY_X_5_ALIAS_443", "YEDA_KEY_X_5"]);
    }

    #[test]
    fn the_alias_variable_wins() {
        let account = super::provider_account("a", "https://env-test.example");
        // SAFETY: no other test reads these variables.
        unsafe {
            std::env::set_var("YEDA_KEY_ENV__TEST_EXAMPLE_443", "shared");
            assert_eq!(super::from_env(&account, Some("one")).as_deref(), Some("shared"));
            std::env::set_var("YEDA_KEY_ENV__TEST_EXAMPLE_443_ALIAS_ONE", "own");
        }
        assert_eq!(super::from_env(&account, Some("one")).as_deref(), Some("own"));
        assert_eq!(super::from_env(&account, Some("two")).as_deref(), Some("shared"));
        assert_eq!(super::from_env(&account, None).as_deref(), Some("shared"));
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
