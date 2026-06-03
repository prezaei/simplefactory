//! A minimal Azure **Managed Identity** [`TokenCredential`] (MSI/IMDS) shared across
//! Factory services (P8): `libs/blob` uses it for Blob `StorageCredentials`, and the
//! dispatcher (P8 part 2) will use it for an ARM token to start Container Apps Jobs.
//!
//! It speaks the two MSI token protocols Factory's runtimes use:
//! - **Container Apps / App Service**: the per-container MSI endpoint advertised via the
//!   `IDENTITY_ENDPOINT` + `IDENTITY_HEADER` env vars (header `X-IDENTITY-HEADER`).
//! - **IMDS** (VM/VMSS) fallback: `http://169.254.169.254/...` with header `Metadata: true`.
//!
//! Implemented directly over the rustls `reqwest` already in the tree rather than pulling
//! `azure_identity`, whose default `oauth2`→native-tls dependency would drag openssl back
//! into a stack that deliberately drops it (rustls + `hmac_rust`). The request
//! *construction* + token *parsing* are pure, unit-tested fns; the network call + token
//! cache are a **deploy seam** (prod-only — CI/local always uses Azurite shared-key, so
//! this code path is never exercised in tests).
#![forbid(unsafe_code)]

use std::sync::Mutex;
use std::time::Duration;

use azure_core::auth::{AccessToken, TokenCredential};
use azure_core::error::{Error as AzureError, ErrorKind};
use time::OffsetDateTime;

/// Default OAuth resource for Azure Storage when the caller's scope doesn't specify one.
const STORAGE_RESOURCE: &str = "https://storage.azure.com/";
/// Refresh a cached token this many seconds before its real expiry (clock skew + the
/// in-flight request window), so a call never uses a token that expires mid-flight.
const EXPIRY_SKEW_SECS: i64 = 300;
/// Per-request timeout for the MSI/IMDS call. The IMDS link-local address (169.254.169.254)
/// blackholes off-Azure / when firewalled (no route, no RST), so without a timeout
/// `get_token` would hang forever and wedge every blob op. Azure's own IMDS guidance
/// mandates a client-side timeout.
const TOKEN_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Managed Identity credential. `client_id` selects a specific **user-assigned** identity
/// (Factory gives each service its own); `None` uses the system-assigned identity.
pub struct ManagedIdentityCredential {
    client_id: Option<String>,
    http: reqwest::Client,
    /// Cache the last token until it nears expiry — MSI endpoints rate-limit, and every
    /// call would otherwise re-fetch.
    cached: Mutex<Option<AccessToken>>,
}

impl std::fmt::Debug for ManagedIdentityCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the token; `client_id` is not a secret.
        f.debug_struct("ManagedIdentityCredential")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

impl ManagedIdentityCredential {
    pub fn new(client_id: Option<String>) -> Self {
        Self {
            client_id,
            http: reqwest::Client::new(),
            cached: Mutex::new(None),
        }
    }
}

/// A built MSI/IMDS token request: base URL + query pairs (encoded by reqwest) + the one
/// auth header. Pure data so the protocol selection is unit-tested without the network.
/// `Debug` redacts `header_value` — on the Container Apps path it carries the MSI secret
/// (`X-IDENTITY-HEADER`), so a stray `debug!(?req)` must not leak it.
#[derive(PartialEq, Eq)]
struct TokenRequest {
    base_url: String,
    query: Vec<(String, String)>,
    header_name: String,
    header_value: String,
}

impl std::fmt::Debug for TokenRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenRequest")
            .field("base_url", &self.base_url)
            .field("query", &self.query)
            .field("header_name", &self.header_name)
            .field("header_value", &"<REDACTED>")
            .finish()
    }
}

/// Choose the MSI protocol from the environment and build the token request. When the
/// Container Apps / App Service identity env (`IDENTITY_ENDPOINT` + `IDENTITY_HEADER`) is
/// present, use that endpoint; otherwise fall back to IMDS. `client_id` (user-assigned MI)
/// is appended when set. Pure — the caller passes the env values in.
fn build_token_request(
    identity_endpoint: Option<&str>,
    identity_header: Option<&str>,
    client_id: Option<&str>,
    resource: &str,
) -> TokenRequest {
    let (base_url, api_version, header_name, header_value) =
        match (identity_endpoint, identity_header) {
            (Some(endpoint), Some(header)) => (
                endpoint.to_string(),
                "2019-08-01",
                "X-IDENTITY-HEADER".to_string(),
                header.to_string(),
            ),
            _ => (
                "http://169.254.169.254/metadata/identity/oauth2/token".to_string(),
                "2018-02-01",
                "Metadata".to_string(),
                "true".to_string(),
            ),
        };
    let mut query = vec![
        ("api-version".to_string(), api_version.to_string()),
        ("resource".to_string(), resource.to_string()),
    ];
    if let Some(c) = client_id {
        query.push(("client_id".to_string(), c.to_string()));
    }
    TokenRequest {
        base_url,
        query,
        header_name,
        header_value,
    }
}

/// The MSI/IMDS resource for a set of OAuth scopes: drop the `.default` suffix Azure SDK
/// scopes carry (the resource keeps its trailing slash), defaulting to the storage
/// resource. Pure + unit-tested.
fn resource_for_scopes(scopes: &[&str]) -> String {
    // An Azure scope is `{resource}.default`, e.g. `https://storage.azure.com/.default`
    // → resource `https://storage.azure.com/`.
    scopes
        .first()
        .map(|s| s.trim_end_matches(".default"))
        .filter(|s| !s.is_empty())
        .unwrap_or(STORAGE_RESOURCE)
        .to_string()
}

/// Parse the MSI/IMDS token response into an [`AccessToken`]. Handles both `expires_on`
/// (absolute epoch seconds, as a string or number) and `expires_in` (seconds from now);
/// applies [`EXPIRY_SKEW_SECS`] so a near-expiry token is treated as already due. Pure +
/// unit-tested (the `now` is injected).
fn parse_token_response(body: &str, now_unix: i64) -> Result<AccessToken, AzureError> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| AzureError::message(ErrorKind::Credential, format!("MSI token JSON: {e}")))?;
    let token = v
        .get("access_token")
        .and_then(|t| t.as_str())
        .ok_or_else(|| AzureError::message(ErrorKind::Credential, "MSI token: no access_token"))?
        .to_string();

    let expires_unix = expires_on_unix(&v, now_unix)
        .ok_or_else(|| AzureError::message(ErrorKind::Credential, "MSI token: no usable expiry"))?;
    let expires_on = OffsetDateTime::from_unix_timestamp(expires_unix - EXPIRY_SKEW_SECS)
        .map_err(|e| AzureError::message(ErrorKind::Credential, format!("MSI expiry: {e}")))?;
    Ok(AccessToken::new(token, expires_on))
}

/// Absolute expiry (epoch seconds) from an MSI response: prefer `expires_on` (string or
/// number), else `now + expires_in`.
fn expires_on_unix(v: &serde_json::Value, now_unix: i64) -> Option<i64> {
    if let Some(eo) = v.get("expires_on") {
        if let Some(n) = eo.as_i64() {
            return Some(n);
        }
        if let Some(s) = eo.as_str() {
            if let Ok(n) = s.parse::<i64>() {
                return Some(n);
            }
        }
    }
    let ei = v.get("expires_in")?;
    let secs = ei
        .as_i64()
        .or_else(|| ei.as_str().and_then(|s| s.parse::<i64>().ok()))?;
    Some(now_unix + secs)
}

#[async_trait::async_trait]
impl TokenCredential for ManagedIdentityCredential {
    async fn get_token(&self, scopes: &[&str]) -> azure_core::Result<AccessToken> {
        // Serve a cached token until it nears expiry. Recover from a poisoned mutex
        // (the critical section only clones/assigns, so the data is always consistent)
        // rather than panicking and turning a recoverable state into a credential outage.
        if let Some(tok) = self
            .cached
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            if tok.expires_on > OffsetDateTime::now_utc() {
                return Ok(tok.clone());
            }
        }

        let resource = resource_for_scopes(scopes);
        // Treat an empty env value as UNSET (a misconfigured/blank secret injection must
        // not select the Container Apps protocol with an empty X-IDENTITY-HEADER — that
        // would fail obscurely; fall cleanly back to IMDS instead).
        let identity_endpoint = std::env::var("IDENTITY_ENDPOINT")
            .ok()
            .filter(|s| !s.is_empty());
        let identity_header = std::env::var("IDENTITY_HEADER")
            .ok()
            .filter(|s| !s.is_empty());
        let req = build_token_request(
            identity_endpoint.as_deref(),
            identity_header.as_deref(),
            self.client_id.as_deref(),
            &resource,
        );
        tracing::info!(
            base_url = %req.base_url,
            client_id = ?self.client_id,
            "managed identity: requesting token"
        );
        let resp = self
            .http
            .get(&req.base_url)
            .query(&req.query)
            .header(&req.header_name, &req.header_value)
            .timeout(TOKEN_REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| {
                AzureError::message(ErrorKind::Credential, format!("MSI token request: {e}"))
            })?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| {
            AzureError::message(ErrorKind::Credential, format!("MSI token body: {e}"))
        })?;
        if !status.is_success() {
            return Err(AzureError::message(
                ErrorKind::Credential,
                format!("MSI token endpoint returned {status}"),
            ));
        }
        let token = parse_token_response(&body, OffsetDateTime::now_utc().unix_timestamp())?;
        *self.cached.lock().unwrap_or_else(|e| e.into_inner()) = Some(token.clone());
        Ok(token)
    }

    async fn clear_cache(&self) -> azure_core::Result<()> {
        *self.cached.lock().unwrap_or_else(|e| e.into_inner()) = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_apps_endpoint_used_when_identity_env_present() {
        let req = build_token_request(
            Some("http://localhost:42356/msi/token"),
            Some("secret-header"),
            Some("client-123"),
            "https://storage.azure.com/",
        );
        assert_eq!(req.base_url, "http://localhost:42356/msi/token");
        assert_eq!(req.header_name, "X-IDENTITY-HEADER");
        assert_eq!(req.header_value, "secret-header");
        assert!(req
            .query
            .contains(&("api-version".to_string(), "2019-08-01".to_string())));
        assert!(req
            .query
            .contains(&("client_id".to_string(), "client-123".to_string())));
        assert!(req.query.contains(&(
            "resource".to_string(),
            "https://storage.azure.com/".to_string()
        )));
    }

    #[test]
    fn imds_fallback_when_no_identity_env() {
        let req = build_token_request(None, None, None, "https://storage.azure.com/");
        assert_eq!(
            req.base_url,
            "http://169.254.169.254/metadata/identity/oauth2/token"
        );
        assert_eq!(req.header_name, "Metadata");
        assert_eq!(req.header_value, "true");
        assert!(req
            .query
            .contains(&("api-version".to_string(), "2018-02-01".to_string())));
        // No client_id when the system-assigned identity is used.
        assert!(!req.query.iter().any(|(k, _)| k == "client_id"));
    }

    #[test]
    fn token_request_debug_redacts_the_identity_header() {
        let req = build_token_request(
            Some("http://localhost/msi"),
            Some("super-secret-header"),
            None,
            "r",
        );
        let dbg = format!("{req:?}");
        assert!(
            !dbg.contains("super-secret-header"),
            "header value must be redacted"
        );
        assert!(dbg.contains("<REDACTED>"));
    }

    #[test]
    fn resource_strips_default_suffix_and_defaults() {
        assert_eq!(
            resource_for_scopes(&["https://storage.azure.com/.default"]),
            "https://storage.azure.com/"
        );
        assert_eq!(
            resource_for_scopes(&["https://vault.azure.net"]),
            "https://vault.azure.net"
        );
        assert_eq!(resource_for_scopes(&[]), STORAGE_RESOURCE);
    }

    #[test]
    fn parses_expires_on_string_and_number() {
        let tok = parse_token_response(
            r#"{"access_token":"abc","expires_on":"2000000000"}"#,
            1_000_000_000,
        )
        .unwrap();
        assert_eq!(tok.token.secret(), "abc");
        assert_eq!(
            tok.expires_on.unix_timestamp(),
            2_000_000_000 - EXPIRY_SKEW_SECS
        );

        let tok2 =
            parse_token_response(r#"{"access_token":"x","expires_on":2000000000}"#, 0).unwrap();
        assert_eq!(
            tok2.expires_on.unix_timestamp(),
            2_000_000_000 - EXPIRY_SKEW_SECS
        );
    }

    #[test]
    fn parses_expires_in_relative() {
        let tok =
            parse_token_response(r#"{"access_token":"x","expires_in":"3600"}"#, 1_000_000).unwrap();
        assert_eq!(
            tok.expires_on.unix_timestamp(),
            1_000_000 + 3600 - EXPIRY_SKEW_SECS
        );
    }

    #[test]
    fn missing_token_or_expiry_is_an_error() {
        assert!(parse_token_response(r#"{"expires_on":"1"}"#, 0).is_err());
        assert!(parse_token_response(r#"{"access_token":"x"}"#, 0).is_err());
        assert!(parse_token_response("not json", 0).is_err());
    }
}
