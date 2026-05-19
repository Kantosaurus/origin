//! In-process memory backend.
//!
//! Used for tests, CI, and any environment where the user opts out of the
//! platform keyring via `ORIGIN_KEYVAULT=memory`. Values live only for the
//! lifetime of the process and are dropped (not zeroized — that contract is
//! on [`crate::Secret`]) when the backend itself drops.

use std::collections::BTreeMap;

use async_trait::async_trait;
use parking_lot::Mutex;

use crate::{backend::Backend, Error};

#[derive(Default)]
pub struct MemoryBackend {
    // Key is (provider, account); value is the raw bytes the caller stored.
    store: Mutex<BTreeMap<(String, String), Vec<u8>>>,
}

impl MemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Backend for MemoryBackend {
    async fn set(&self, provider: &str, account: &str, value: &[u8]) -> Result<(), Error> {
        self.store
            .lock()
            .insert((provider.to_owned(), account.to_owned()), value.to_vec());
        Ok(())
    }

    async fn get(&self, provider: &str, account: &str) -> Result<Vec<u8>, Error> {
        self.store
            .lock()
            .get(&(provider.to_owned(), account.to_owned()))
            .cloned()
            .ok_or_else(|| Error::NotFound {
                provider: provider.to_owned(),
                account: account.to_owned(),
            })
    }

    async fn delete(&self, provider: &str, account: &str) -> Result<(), Error> {
        let removed = self
            .store
            .lock()
            .remove(&(provider.to_owned(), account.to_owned()));
        if removed.is_some() {
            Ok(())
        } else {
            Err(Error::NotFound {
                provider: provider.to_owned(),
                account: account.to_owned(),
            })
        }
    }

    async fn list(&self, provider: &str) -> Result<Vec<String>, Error> {
        let mut out: Vec<String> = {
            let guard = self.store.lock();
            guard
                .keys()
                .filter(|(p, _)| p == provider)
                .map(|(_, a)| a.clone())
                .collect()
        };
        out.sort();
        Ok(out)
    }
}
