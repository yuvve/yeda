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

/// The secret stored for `account`, or `None` if there is none.
pub fn get(account: &str) -> Result<Option<String>> {
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

/// The account name for a git remote's password or access token.
pub fn git_account(url: &str) -> String {
    format!("git:{url}")
}

/// The account name for an LLM API key. It names the server, so a shared config that points a
/// profile elsewhere finds no key to send there.
pub fn llm_account(url: &str) -> String {
    format!("llm:{url}")
}

#[cfg(test)]
mod tests {
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
        for account in [super::webdav_account("yeda-test", "https://example.invalid/dav"), super::llm_account("https://example.invalid/v1")] {
            super::set(&account, "s3cret").unwrap();
            assert_eq!(super::get(&account).unwrap().as_deref(), Some("s3cret"));
            super::delete(&account).unwrap();
            assert_eq!(super::get(&account).unwrap(), None);
        }
    }
}
