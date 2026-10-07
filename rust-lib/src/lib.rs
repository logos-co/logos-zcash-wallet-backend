//! zcash_wallet_backend: the coordinator between the Zcash wallet surfaces and the engine.
//!
//! It keeps roles, follows the engine's jobs, relays its events and passes the
//! node module's routes to it. It holds no key material and caches no password.
//! The pure parts are tested with `cargo test --no-default-features`.

pub mod gate;
pub mod model;

#[cfg(feature = "logos_module")]
mod glue;
