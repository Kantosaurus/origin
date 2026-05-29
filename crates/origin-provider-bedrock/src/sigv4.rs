// SPDX-License-Identifier: Apache-2.0
//! `SigV4` request signing for AWS Bedrock `InvokeModel`.
//!
//! Wraps `aws-sigv4` to produce the small bundle of additional headers
//! (`Authorization`, `x-amz-date`, `x-amz-content-sha256`) that the daemon
//! attaches to the outgoing `reqwest::RequestBuilder`.
#![allow(clippy::module_name_repetitions, clippy::redundant_pub_crate)]

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4;
use std::time::SystemTime;

/// Compute the `SigV4` headers (`Authorization`, `x-amz-date`, ...) for a Bedrock
/// `InvokeModel` request. Returns owned `(name, value)` pairs ready to attach
/// to a `reqwest::RequestBuilder`.
///
/// # Errors
/// Returns a string describing the failure if credential, settings, or signing
/// construction fails (typically a malformed URL or empty key).
pub(crate) fn signed_headers(
    method: &str,
    url: &str,
    body: &[u8],
    region: &str,
    access_key: &str,
    secret_key: &str,
) -> Result<Vec<(String, String)>, String> {
    let identity = Credentials::new(access_key, secret_key, None, None, "origin-bedrock").into();
    let settings = SigningSettings::default();
    let params = v4::SigningParams::builder()
        .identity(&identity)
        .region(region)
        .name("bedrock")
        .time(SystemTime::now())
        .settings(settings)
        .build()
        .map_err(|e| e.to_string())?
        .into();

    let signable = SignableRequest::new(method, url, std::iter::empty(), SignableBody::Bytes(body))
        .map_err(|e| e.to_string())?;
    let (instructions, _signature) = sign(signable, &params).map_err(|e| e.to_string())?.into_parts();
    let (headers, _new_query) = instructions.into_parts();
    Ok(headers
        .into_iter()
        .map(|h| (h.name().to_string(), h.value().to_string()))
        .collect())
}
