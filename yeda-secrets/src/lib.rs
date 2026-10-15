//! Secrets (WebDAV passwords, LLM API keys) in the system's secret store: the Secret Service on
//! Linux (GNOME Keyring, KWallet, KeePassXC) and the Keystore on Android. Never in a file.

use anyhow::{Context, Result};
use keyring_core::{Entry, Error};
use std::sync::Once;

const SERVICE: &str = "yeda";

fn entry(account: &str) -> Result<Entry> {
    static STORE: Once = Once::new();
    let mut failed = None;
    STORE.call_once(|| match store() {
        Ok(store) => keyring_core::set_default_store(store),
        Err(e) => failed = Some(e),
    });
    if let Some(e) = failed {
        return Err(e).context("opening the system secret store");
    }
    Entry::new(SERVICE, account).context("opening the system secret store")
}

#[cfg(target_os = "android")]
fn store() -> keyring_core::Result<std::sync::Arc<keyring_core::CredentialStore>> {
    Ok(android_native_keyring_store::Store::new()?)
}

#[cfg(not(target_os = "android"))]
fn store() -> keyring_core::Result<std::sync::Arc<keyring_core::CredentialStore>> {
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

/// The account name for an LLM API key, named after the environment variable that can also hold it.
pub fn llm_account(env: &str) -> String {
    format!("llm:{env}")
}
