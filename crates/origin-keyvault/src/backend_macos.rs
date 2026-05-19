//! macOS backend — wraps the Security framework's generic password APIs.
//!
//! Service name is namespaced as `origin/{provider}`; the account string
//! is passed through verbatim. All sync FFI calls are dispatched to
//! `spawn_blocking` so they never stall the async runtime.
//!
//! `list` is a stub for now and returns an empty `Vec`; Phase 11 will wire
//! `SecItemCopyMatching` for proper enumeration.

use async_trait::async_trait;
use security_framework::passwords::{delete_generic_password, get_generic_password, set_generic_password};
use tokio::task::spawn_blocking;

use crate::{backend::Backend, Error};

pub struct MacBackend;

impl MacBackend {
    pub const fn new() -> Self {
        Self
    }
}

fn service_name(provider: &str) -> String {
    format!("origin/{provider}")
}

fn join_err<E: std::fmt::Display>(e: E) -> Error {
    Error::Backend(format!("join: {e}"))
}

#[async_trait]
impl Backend for MacBackend {
    async fn set(&self, provider: &str, account: &str, value: &[u8]) -> Result<(), Error> {
        let svc = service_name(provider);
        let acc = account.to_owned();
        let val = value.to_vec();
        spawn_blocking(move || {
            set_generic_password(&svc, &acc, &val)
                .map_err(|e| Error::Backend(format!("set_generic_password: {e}")))
        })
        .await
        .map_err(join_err)?
    }

    async fn get(&self, provider: &str, account: &str) -> Result<Vec<u8>, Error> {
        let svc = service_name(provider);
        let acc = account.to_owned();
        let p_owned = provider.to_owned();
        let a_owned = account.to_owned();
        spawn_blocking(move || {
            get_generic_password(&svc, &acc).map_err(|e| {
                // Treat any retrieval failure as NotFound; the macOS error
                // type does not expose a stable "missing" discriminant
                // across versions, and the caller will retry on transient
                // keychain prompts.
                let msg = format!("{e}");
                if msg.contains("-25300") || msg.to_lowercase().contains("not found") {
                    Error::NotFound {
                        provider: p_owned,
                        account: a_owned,
                    }
                } else {
                    Error::Backend(format!("get_generic_password: {e}"))
                }
            })
        })
        .await
        .map_err(join_err)?
    }

    async fn delete(&self, provider: &str, account: &str) -> Result<(), Error> {
        let svc = service_name(provider);
        let acc = account.to_owned();
        let p_owned = provider.to_owned();
        let a_owned = account.to_owned();
        spawn_blocking(move || {
            delete_generic_password(&svc, &acc).map_err(|e| {
                let msg = format!("{e}");
                if msg.contains("-25300") || msg.to_lowercase().contains("not found") {
                    Error::NotFound {
                        provider: p_owned,
                        account: a_owned,
                    }
                } else {
                    Error::Backend(format!("delete_generic_password: {e}"))
                }
            })
        })
        .await
        .map_err(join_err)?
    }

    async fn list(&self, _provider: &str) -> Result<Vec<String>, Error> {
        // P11 will wire `SecItemCopyMatching` for proper enumeration.
        Ok(Vec::new())
    }
}
