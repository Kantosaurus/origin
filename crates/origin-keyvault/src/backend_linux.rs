// SPDX-License-Identifier: Apache-2.0
//! Linux backend — talks to the freedesktop Secret Service (gnome-keyring,
//! `KWallet`, etc.) over D-Bus.
//!
//! Items are stored under attribute pairs:
//!   `origin-provider = <provider>`
//!   `origin-account  = <account>`
//!
//! `list(provider)` enumerates by `origin-provider` and returns the
//! `origin-account` attribute of each match.

use std::collections::HashMap;

use async_trait::async_trait;
use secret_service::{EncryptionType, SecretService};

use crate::{backend::Backend, Error};

const ATTR_PROVIDER: &str = "origin-provider";
const ATTR_ACCOUNT: &str = "origin-account";
const MIME_PLAIN: &str = "text/plain";

pub struct LinuxBackend;

impl LinuxBackend {
    pub const fn new() -> Self {
        Self
    }

    async fn service() -> Result<SecretService<'static>, Error> {
        SecretService::connect(EncryptionType::Dh)
            .await
            .map_err(|e| Error::Backend(format!("secret-service connect: {e}")))
    }

    fn attrs<'a>(provider: &'a str, account: &'a str) -> HashMap<&'a str, &'a str> {
        let mut m = HashMap::new();
        m.insert(ATTR_PROVIDER, provider);
        m.insert(ATTR_ACCOUNT, account);
        m
    }
}

#[async_trait]
impl Backend for LinuxBackend {
    async fn set(&self, provider: &str, account: &str, value: &[u8]) -> Result<(), Error> {
        let svc = Self::service().await?;
        let coll = svc
            .get_default_collection()
            .await
            .map_err(|e| Error::Backend(format!("default collection: {e}")))?;
        let label = format!("origin/{provider}/{account}");
        coll.create_item(&label, Self::attrs(provider, account), value, true, MIME_PLAIN)
            .await
            .map_err(|e| Error::Backend(format!("create_item: {e}")))?;
        Ok(())
    }

    async fn get(&self, provider: &str, account: &str) -> Result<Vec<u8>, Error> {
        let svc = Self::service().await?;
        let items = svc
            .search_items(Self::attrs(provider, account))
            .await
            .map_err(|e| Error::Backend(format!("search_items: {e}")))?;
        let item = items.unlocked.into_iter().next().ok_or_else(|| Error::NotFound {
            provider: provider.to_owned(),
            account: account.to_owned(),
        })?;
        item.get_secret()
            .await
            .map_err(|e| Error::Backend(format!("get_secret: {e}")))
    }

    async fn delete(&self, provider: &str, account: &str) -> Result<(), Error> {
        let svc = Self::service().await?;
        let items = svc
            .search_items(Self::attrs(provider, account))
            .await
            .map_err(|e| Error::Backend(format!("search_items: {e}")))?;
        let item = items.unlocked.into_iter().next().ok_or_else(|| Error::NotFound {
            provider: provider.to_owned(),
            account: account.to_owned(),
        })?;
        item.delete()
            .await
            .map_err(|e| Error::Backend(format!("delete: {e}")))?;
        Ok(())
    }

    async fn list(&self, provider: &str) -> Result<Vec<String>, Error> {
        let svc = Self::service().await?;
        let mut attrs = HashMap::new();
        attrs.insert(ATTR_PROVIDER, provider);
        let items = svc
            .search_items(attrs)
            .await
            .map_err(|e| Error::Backend(format!("search_items: {e}")))?;
        let mut out = Vec::new();
        for item in items.unlocked.into_iter().chain(items.locked) {
            let item_attrs = item
                .get_attributes()
                .await
                .map_err(|e| Error::Backend(format!("get_attributes: {e}")))?;
            if let Some(account) = item_attrs.get(ATTR_ACCOUNT) {
                out.push(account.clone());
            }
        }
        out.sort();
        out.dedup();
        Ok(out)
    }
}
