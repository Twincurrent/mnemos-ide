use http::StatusCode;

use super::{register_error, ErrorExt};

impl ErrorExt for reqwest::Error {
    fn is_actionable(&self) -> bool {
        // Outside of timeouts, there's nothing we can do about errors
        // that occur prior to the successful receipt of an HTTP
        // response.

        // There's no way to check for connection errors via web APIs, so
        // `is_connect` can only be called on native platforms.
        #[cfg(not(target_family = "wasm"))]
        if self.is_connect() {
            return false;
        }

        if self.is_request() || self.is_body() || self.is_decode() {
            return false;
        }

        // If we're getting a capacity error from the server, then that should trip a server-side
        // alert. A duplicate report in Sentry isn't helpful.
        if self.status() == Some(StatusCode::TOO_MANY_REQUESTS) {
            return false;
        }

        // Internal server errors (5xx) are server-side issues that we can't act upon from the client.
        if self.status().is_some_and(|status| status.is_server_error()) {
            return false;
        }

        // Mnemos IDE: upstream downgraded 403s from Warp's staging server here; Mnemos has no
        // staging server, so that special case is gone.
        true
    }
}
register_error!(reqwest::Error);
