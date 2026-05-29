// SPDX-License-Identifier: Apache-2.0
//! Internal `Backend` trait shared by every per-OS implementation.
//!
//! Implementations are crate-private. The public surface is the
//! [`crate::KeyVault`] façade, which adapts `Vec<u8>` results into
//! `Secret<String>` via UTF-8 validation.

use async_trait::async_trait;

use crate::Error;

#[async_trait]
pub trait Backend: Send + Sync {
    async fn set(&self, provider: &str, account: &str, value: &[u8]) -> Result<(), Error>;
    async fn get(&self, provider: &str, account: &str) -> Result<Vec<u8>, Error>;
    async fn delete(&self, provider: &str, account: &str) -> Result<(), Error>;
    async fn list(&self, provider: &str) -> Result<Vec<String>, Error>;
}
