//! A minimal Azure **Managed Identity** [`TokenCredential`] (P8) — the prod credential
//! the blob stores swap to (Azurite/dev keeps shared-key). It speaks the two MSI token
//! protocols Factory's runtimes use:
//!
//! - **Container Apps / App Service**: the per-container MSI endpoint advertised via the
//!   `IDENTITY_ENDPOINT` + `IDENTITY_HEADER` env vars (header `X-IDENTITY-HEADER`).
//! - **IMDS** (VM/VMSS) fallback: `http://169.254.169.254/...` with header `Metadata: true`.
//!
//! It is implemented directly over the rustls `reqwest` already in the tree rather than
//! pulling `azure_identity`, whose default `oauth2`→native-tls dependency would drag
//! openssl back into a stack that deliberately drops it (rustls + `hmac_rust`). The
//! request *construction* is a pure, unit-tested fn ([`build_token_request`]); the network
//! call + token cache is a **deploy seam** (prod-only — CI/local always uses Azurite
//! shared-key, so this code path is never exercised in tests).

use std::sync::Mutex;

use azure_core::auth::{AccessToken, TokenCredential};
use azure_core::error::{Error as AzureError, ErrorKind};
use time::OffsetDateTime;

/// Default OAuth resource for Azure Storage when the caller's scope doesn't specify one.
const STORAGE_RESOURCE: &str = "https://storage.azure.com/";
/// Refresh a cached token this many seconds before its real expiry (clock skew + the
/// in-flight request window), so a call never uses a token that expires mid-flight.
const EXPIRY_SKEW_SECS: i64 = 300;

/// Managed Identity credential. `client_id` selects a specific **user-assigned** identity
/// (Factory gives each service its own); `None` uses the system-assigned identity.
pub(crate) struct ManagedIdentityCredential {
    client_id: Option<String>,
    http: reqwest::Client,
    /// Cache the last token until it nears expiry — MSI endpoints rate-limit, and every
    /// blob op would otherwise re-fetch.
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
    pub(crate) fn new(client_id: Option<String>) -> Self {
        Self {
            client_id,
            http: reqwest::Client::new(),
            cached: Mutex::new(None),
        }
    }
}

/// A built MSI/IMDS token request: base URL + query pairs (encoded by reqwest) + the one
/// auth header. Pure data so the protocol selection is unit-tested without the network.
#[derive(Debug, PartialEq, Eq)]
struct TokenRequest {
    base_url: String,
    query: Vec<(String, String)>,
    header_name: String,
    header_value: String,
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
    match (identity_endpoint, identity_header) {
        (Some(endpoint), Some(_header)) => {
            let mut query = vec![
                ("api-version".to_string(), "2019-08-01".to_string()),
                ("resource".to_string(), resource.to_string()),
            ];
            if let Some(c) = client_id {
                query.push(("client_id".to_string(), c.to_string()));
            }
            TokenRequest {
                base_url: endpoint.to_string(),
                query,
                header_name: "X-IDENTITY-HEADER".to_string(),
                header_value: identity_header.unwrap_or_default().to_string(),
            }
        }
        _ => {
            let mut query = vec![
                ("api-version".to_string(), "2018-02-01".to_string()),
                ("resource".to_string(), resource.to_string()),
            ];
            if let Some(c) = client_id {
                query.push(("client_id".to_string(), c.to_string()));
            }
            TokenRequest {
                base_url: "http://169.254.169.254/metadata/identity/oauth2/token".to_string(),
                query,
                header_name: "Metadata".to_string(),
                header_value: "true".to_string(),
            }
        }
    }
}

/// The MSI/IMDS resource for a set of OAuth scopes: drop the `/.default` suffix Azure SDK
/// scopes carry (the MSI `resource` param wants the bare audience), defaulting to the
/// storage resource. Pure + unit-tested.
fn resource_for_scopes(scopes: &[&str]) -> String {
    // An Azure scope is `{resource}.default` (the resource keeps its trailing slash),
    // e.g. `https://storage.azure.com/.default` → resource `https://storage.azure.com/`.
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
        // Serve a cached token until it nears expiry.
        if let Some(tok) = self.cached.lock().expect("token cache mutex").as_ref() {
            if tok.expires_on > OffsetDateTime::now_utc() {
                return Ok(tok.clone());
            }
        }

        let resource = resource_for_scopes(scopes);
        let req = build_token_request(
            std::env::var("IDENTITY_ENDPOINT").ok().as_deref(),
            std::env::var("IDENTITY_HEADER").ok().as_deref(),
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
        *self.cached.lock().expect("token cache mutex") = Some(token.clone());
        Ok(token)
    }

    async fn clear_cache(&self) -> azure_core::Result<()> {
        *self.cached.lock().expect("token cache mutex") = None;
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
        // expires_on as a string of epoch seconds (Container Apps / IMDS shape).
        let tok = parse_token_response(
            r#"{"access_token":"abc","expires_on":"2000000000"}"#,
            1_000_000_000,
        )
        .unwrap();
        assert_eq!(tok.token.secret(), "abc");
        // Expiry is the stated time minus the skew.
        assert_eq!(
            tok.expires_on.unix_timestamp(),
            2_000_000_000 - EXPIRY_SKEW_SECS
        );

        // expires_on as a JSON number.
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
