// SPDX-License-Identifier: AGPL-3.0-only
//! Desktop sign-in with PKCE (RFC 7636, S256).
//!
//! The app opens the sync server's `/signup/remote` page with a
//! `code_challenge`. After the user signs in there, the browser hands the app
//! `mnemos://auth/desktop_redirect?code=...&state=...`: a one-time code that
//! lives for a couple of minutes on the server, not a token. The app then
//! trades the code for the token pair with `POST /api/auth/desktop-exchange`,
//! proving with the `code_verifier` that it is the process that started the
//! sign-in. A code read from a browser history, a log or another app that
//! registered the `mnemos` scheme is worthless without the verifier, which
//! never leaves this process.
//!
//! A server from before PKCE ignores the challenge and still redirects with
//! the tokens in the URL; the app keeps accepting that redirect until the
//! server drops it (see `LEGACY_TOKEN_REDIRECT_REMOVAL` in the Admin repo).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{Duration, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::auth::{AdminTokens, LoginResponse};
use crate::error::IdeError;

/// The only challenge method the app sends and the server accepts.
pub const CODE_CHALLENGE_METHOD: &str = "S256";

/// Path of the code exchange, relative to the server root.
pub const EXCHANGE_PATH: &str = "/api/auth/desktop-exchange";

/// A PKCE verifier for one desktop sign-in. Kept in memory only, next to the
/// sign-in's `state`, and dropped once the redirect is handled.
#[derive(Clone, PartialEq, Eq)]
pub struct CodeVerifier(String);

impl std::fmt::Debug for CodeVerifier {
    // The verifier is a secret for as long as its code is live.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("CodeVerifier(..)")
    }
}

impl CodeVerifier {
    /// A fresh verifier: 32 bytes from the OS random source (two v4 UUIDs,
    /// whose 122 random bits each come from `getrandom`), base64url without
    /// padding, so 43 characters from RFC 7636's unreserved set.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// Wrap a verifier that came from elsewhere (tests), refusing one RFC
    /// 7636 would refuse.
    pub fn parse(value: &str) -> Option<Self> {
        is_valid_verifier(value).then(|| Self(value.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The S256 challenge sent with the browser sign-in.
    pub fn challenge(&self) -> String {
        code_challenge_s256(&self.0)
    }
}

/// `BASE64URL(SHA256(verifier))` without padding (RFC 7636 section 4.2).
pub fn code_challenge_s256(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// RFC 7636 section 4.1: 43 to 128 characters of `[A-Za-z0-9-._~]`.
pub fn is_valid_verifier(value: &str) -> bool {
    (43..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

/// A one-time code as the redirect carries it: base64url, of a sane length.
/// Anything else is refused before it is sent anywhere.
pub fn is_plausible_code(value: &str) -> bool {
    (16..=256).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

#[derive(Debug, Serialize)]
struct ExchangeRequest<'a> {
    code: &'a str,
    code_verifier: &'a str,
}

/// The URL of the code exchange on the server at `issuer`.
pub fn exchange_url(issuer: &str) -> String {
    format!("{}{EXCHANGE_PATH}", issuer.trim_end_matches('/'))
}

/// Trade the redirect's one-time `code` for the admin token pair at `issuer`,
/// the server the app sent the user to sign in at (never a value the redirect
/// carries). Runs on mnemos-ide-sync's own tokio runtime, so it can be awaited
/// from the app's executor, which is not tokio.
///
/// Nothing is written to the keyring here; the caller persists the pair the
/// same way it persists a legacy redirect's tokens.
pub async fn exchange_desktop_code(
    issuer: String,
    code: String,
    verifier: CodeVerifier,
) -> Result<AdminTokens, IdeError> {
    if !is_plausible_code(&code) {
        return Err(IdeError::Auth("the sign-in code is malformed".to_string()));
    }
    let join = crate::manager::sync_runtime().spawn(async move {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_default();
        let body = ExchangeRequest {
            code: &code,
            code_verifier: verifier.as_str(),
        };
        let resp = http.post(exchange_url(&issuer)).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(IdeError::Auth(format!(
                "sign-in code exchange failed: {status} {text}"
            )));
        }
        let parsed: LoginResponse = resp.json().await?;
        Ok(tokens_from_response(parsed))
    });
    join.await
        .map_err(|e| IdeError::Auth(format!("sign-in code exchange did not finish: {e}")))?
}

/// The stored shape of an exchange (or login) response.
fn tokens_from_response(parsed: LoginResponse) -> AdminTokens {
    AdminTokens {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_at: Utc::now() + Duration::seconds(parsed.expires_in),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_matches_rfc_7636_appendix_b() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        assert_eq!(
            code_challenge_s256(verifier),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        assert_eq!(
            CodeVerifier::parse(verifier).unwrap().challenge(),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_verifiers_are_valid_and_distinct() {
        let a = CodeVerifier::generate();
        let b = CodeVerifier::generate();
        assert_eq!(a.as_str().len(), 43);
        assert!(is_valid_verifier(a.as_str()));
        assert!(is_valid_verifier(b.as_str()));
        assert_ne!(a, b);
        // The challenge is a base64url SHA-256: 43 chars, no padding.
        let challenge = a.challenge();
        assert_eq!(challenge.len(), 43);
        assert!(!challenge.contains('='));
    }

    #[test]
    fn verifier_parsing_follows_the_rfc() {
        assert!(CodeVerifier::parse(&"a".repeat(42)).is_none());
        assert!(CodeVerifier::parse(&"a".repeat(43)).is_some());
        assert!(CodeVerifier::parse(&"a".repeat(128)).is_some());
        assert!(CodeVerifier::parse(&"a".repeat(129)).is_none());
        assert!(CodeVerifier::parse(&format!("{}+", "a".repeat(43))).is_none());
        assert!(CodeVerifier::parse(&format!("{}~._-", "a".repeat(43))).is_some());
    }

    #[test]
    fn verifier_debug_hides_the_secret() {
        let v = CodeVerifier::generate();
        assert!(!format!("{v:?}").contains(v.as_str()));
    }

    #[test]
    fn code_shape_is_checked_before_sending() {
        assert!(is_plausible_code("abcDEF0123456789-_xyz"));
        assert!(!is_plausible_code("short"));
        assert!(!is_plausible_code("abcdefghijklmnop&x=1"));
        assert!(!is_plausible_code(&"a".repeat(257)));
    }

    #[test]
    fn exchange_url_joins_the_issuer_root() {
        assert_eq!(
            exchange_url("https://admin.example.test/"),
            "https://admin.example.test/api/auth/desktop-exchange"
        );
        assert_eq!(
            exchange_url("https://admin.example.test"),
            "https://admin.example.test/api/auth/desktop-exchange"
        );
    }

    #[test]
    fn exchange_refuses_a_malformed_code_without_a_request() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let err = rt
            .block_on(exchange_desktop_code(
                // Unroutable: a request would fail differently.
                "http://127.0.0.1:9".to_string(),
                "bad code".to_string(),
                CodeVerifier::generate(),
            ))
            .unwrap_err();
        assert!(matches!(err, IdeError::Auth(m) if m.contains("malformed")));
    }

    #[test]
    fn response_becomes_tokens_with_the_server_lifetime() {
        let parsed: LoginResponse = serde_json::from_value(serde_json::json!({
            "access_token": "AT",
            "refresh_token": "RT",
            "token_type": "Bearer",
            "expires_in": 3600,
            "user": {"id": "u", "email": "e@example.test", "profile_id": "p"}
        }))
        .unwrap();
        let tokens = tokens_from_response(parsed);
        assert_eq!(tokens.access_token, "AT");
        assert_eq!(tokens.refresh_token, "RT");
        let left = tokens.expires_at - Utc::now();
        assert!(left > Duration::seconds(3500) && left <= Duration::seconds(3600));
    }
}
