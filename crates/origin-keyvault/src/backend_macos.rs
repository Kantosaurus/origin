//! macOS backend — wraps the Security framework's generic password APIs.
//!
//! Service name is namespaced as `origin/{provider}`; the account string
//! is passed through verbatim. All sync FFI calls are dispatched to
//! `spawn_blocking` so they never stall the async runtime.
//!
//! UNTESTED ON THIS HOST: requires macOS for compile + runtime verification.
//! The `set`/`get`/`delete` paths are unchanged from earlier phases; only
//! `list` is new in Phase 11 and is gated behind a best-effort wrapper
//! around `SecItemCopyMatching` (via `security_framework::item`).

use async_trait::async_trait;
use security_framework::item::{ItemClass, ItemSearchOptions, Limit, SearchResult};
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

/// `errSecItemNotFound` — keychain returns this when the query matches
/// zero entries. We translate it into an empty `Vec` rather than an error.
const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

/// CoreFoundation short-attribute key for `kSecAttrAccount`.
///
/// `security_framework`'s `SearchResult::simplify_dict` returns a
/// `HashMap<String, String>` keyed on the CFString underlying
/// `kSecAttr*` constants. The string value for `kSecAttrAccount` is
/// the well-known four-letter Apple identifier `"acct"`; see
/// `<Security/SecItem.h>` in the Apple SDK.
const K_SEC_ATTR_ACCOUNT_STR: &str = "acct";

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

    // UNTESTED — Phase 11 macOS keychain enumeration. Verify on macOS before relying on the returned set.
    async fn list(&self, provider: &str) -> Result<Vec<String>, Error> {
        let svc = service_name(provider);
        spawn_blocking(move || -> Result<Vec<String>, Error> {
            // Build a query restricted to generic-password items whose
            // `kSecAttrService` equals our namespaced service. Ask the
            // keychain to return the dictionary of attributes for each
            // match (we only need `kSecAttrAccount`, but the API only
            // toggles "load_attributes" as a whole).
            let search_result = ItemSearchOptions::new()
                .class(ItemClass::generic_password())
                .service(&svc)
                .limit(Limit::All)
                .load_attributes(true)
                .search();

            let results = match search_result {
                Ok(r) => r,
                Err(e) => {
                    if e.code() == ERR_SEC_ITEM_NOT_FOUND {
                        return Ok(Vec::new());
                    }
                    return Err(Error::Backend(format!("SecItemCopyMatching: {e}")));
                }
            };

            let mut accounts: Vec<String> = Vec::with_capacity(results.len());
            for r in &results {
                if let SearchResult::Dict(_) = r {
                    if let Some(map) = r.simplify_dict() {
                        if let Some(acct) = map.get(K_SEC_ATTR_ACCOUNT_STR) {
                            accounts.push(acct.clone());
                        }
                    }
                }
            }
            Ok(accounts)
        })
        .await
        .map_err(join_err)?
    }
}
