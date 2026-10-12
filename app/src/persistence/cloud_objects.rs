//! Supporting types for persisting cloud objects to SQLite.

pub use mnemos_server_client::persistence::{decode_guests, decode_link_sharing};

#[cfg(test)]
pub use mnemos_server_client::persistence::encode_guests;

#[cfg(test)]
#[path = "cloud_object_tests.rs"]
mod tests;
