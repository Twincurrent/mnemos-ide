//! Representation of Mnemos user credentials.
//!
//! The primary representation is [`Credentials`], which is the source of truth for how a user is
//! authenticated to Mnemos.
//!
//! Credentials can be split into two halves:
//! * [`LoginToken`], which is a long-lived token that we use to fetch user information.
//!   When using Firebase, this is an OAuth2 refresh token.
//! * [`AuthToken`], which is a short-lived token that's included in all other server requests.
//!   When using Firebase, this is an OAuth2 access token.
use mnemos_graphql::object_permissions::OwnerType;

use super::user::FirebaseAuthTokens;

/// Represents the different ways a user can authenticate with Mnemos.
#[derive(Clone, Debug)]
pub enum Credentials {
    /// Firebase authentication with ID token and refresh token.
    ///
    /// Mnemos IDE: nothing creates this any more (the Firebase sign-in and token exchange are
    /// gone, and `fetch_auth_tokens` refuses it). It stays because it is the in-memory form of
    /// the upstream `PersistedUser` record in secure storage (`AuthState::apply_persisted_user`
    /// and `persist_action`): removing it means changing that stored format and its readers
    /// together, with a migration that still reads and then clears an old record.
    Firebase(FirebaseAuthTokens),
    /// API key for direct server authentication.
    ApiKey {
        key: String,
        /// The owner type for this API key. Only set after user info is fetched from the server.
        owner_type: Option<OwnerType>,
    },
    /// Authentication derived from an ambient browser session cookie.
    SessionCookie,
    /// Test credentials used in unit tests, integration tests, and skip_login builds.
    #[cfg(any(test, feature = "integration_tests", feature = "skip_login"))]
    Test,
}

impl Credentials {
    /// Returns the Firebase auth tokens if this is a Firebase credential.
    pub fn as_firebase(&self) -> Option<&FirebaseAuthTokens> {
        match self {
            Credentials::Firebase(tokens) => Some(tokens),
            Credentials::ApiKey { .. } => None,
            Credentials::SessionCookie => None,
            #[cfg(any(test, feature = "integration_tests", feature = "skip_login"))]
            Credentials::Test => None,
        }
    }

    /// Returns the API key string if this is an API key credential.
    pub fn as_api_key(&self) -> Option<&str> {
        match self {
            Credentials::ApiKey { key, .. } => Some(key),
            Credentials::Firebase(_) => None,
            Credentials::SessionCookie => None,
            #[cfg(any(test, feature = "integration_tests", feature = "skip_login"))]
            Credentials::Test => None,
        }
    }

    /// Returns the owner type if this is an API key credential.
    pub fn api_key_owner_type(&self) -> Option<OwnerType> {
        match self {
            Credentials::ApiKey { owner_type, .. } => *owner_type,
            Credentials::Firebase(_) => None,
            Credentials::SessionCookie => None,
            #[cfg(any(test, feature = "integration_tests", feature = "skip_login"))]
            Credentials::Test => None,
        }
    }

    /// Returns the Firebase refresh token if this is a Firebase credential.
    pub fn refresh_token(&self) -> Option<&str> {
        match self {
            Credentials::Firebase(tokens) => Some(&tokens.refresh_token),
            Credentials::ApiKey { .. } => None,
            Credentials::SessionCookie => None,
            #[cfg(any(test, feature = "integration_tests", feature = "skip_login"))]
            Credentials::Test => None,
        }
    }

    /// Returns the short-lived token to use in HTTP requests to the server.
    pub fn bearer_token(&self) -> AuthToken {
        match self {
            Credentials::Firebase(tokens) => AuthToken::Firebase(tokens.id_token.clone()),
            Credentials::ApiKey { key, .. } => AuthToken::ApiKey(key.clone()),
            Credentials::SessionCookie => AuthToken::NoAuth,
            #[cfg(any(test, feature = "integration_tests", feature = "skip_login"))]
            Credentials::Test => AuthToken::NoAuth,
        }
    }

    /// Get the long-lived login token for these credentials. Returns `None` if there is no such token.
    pub fn login_token(&self) -> Option<LoginToken> {
        match self {
            Credentials::Firebase(tokens) => Some(LoginToken::Firebase(FirebaseToken::Refresh(
                RefreshToken::new(&tokens.refresh_token),
            ))),
            Credentials::ApiKey { key, .. } => Some(LoginToken::ApiKey(key.clone())),
            Credentials::SessionCookie => Some(LoginToken::SessionCookie),
            #[cfg(any(test, feature = "integration_tests", feature = "skip_login"))]
            Credentials::Test => None,
        }
    }
}

/// Represents different types of authentication tokens.
#[derive(Debug, Clone)]
pub enum AuthToken {
    /// Firebase short-lived access token.
    Firebase(String),
    /// API key for direct server authentication.
    ApiKey(String),
    /// No authentication token available (e.g. session cookie auth or test credentials).
    #[cfg_attr(
        not(any(test, feature = "integration_tests", feature = "skip_login")),
        allow(dead_code)
    )]
    NoAuth,
}

impl AuthToken {
    /// Returns the token string to use in an Authorization header, or `None` if auth is not
    /// header-based (e.g. session cookie) or there is no auth.
    pub fn as_bearer_token(&self) -> Option<&str> {
        match self {
            AuthToken::Firebase(token) => Some(token),
            AuthToken::ApiKey(key) => Some(key),
            AuthToken::NoAuth => None,
        }
    }

    /// Returns the bearer token as an owned string, or `None` if auth is not header-based.
    pub fn bearer_token(&self) -> Option<String> {
        match self {
            AuthToken::Firebase(token) => Some(token.clone()),
            AuthToken::ApiKey(key) => Some(key.clone()),
            AuthToken::NoAuth => None,
        }
    }
}

/// Long-lived credentials exchanged for user information.
#[derive(Debug)]
pub enum LoginToken {
    /// A Firebase token to be exchanged for auth tokens.
    Firebase(FirebaseToken),
    /// An API key for direct server authentication.
    ApiKey(String),
    /// Authentication derived from an ambient browser session cookie.
    SessionCookie,
}

/// The type of firebase token that can be used to authenticate a user.
///
/// Mnemos IDE: only the refresh token of a stale upstream login is left; the
/// device-code login that produced the `Custom` variant is gone, and
/// `fetch_auth_tokens` refuses every Firebase token, which sends such a user
/// back to sign-in.
#[derive(Debug)]
pub enum FirebaseToken {
    /// The token type for a logged in user.
    Refresh(RefreshToken),
}

#[derive(Debug, Clone)]
pub struct RefreshToken(String);

impl RefreshToken {
    pub fn new(token: impl Into<String>) -> Self {
        Self(token.into())
    }

    pub fn get(&self) -> &str {
        self.0.as_str()
    }
}
