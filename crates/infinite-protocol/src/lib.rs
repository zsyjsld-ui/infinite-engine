//! Shared native/WASM movement contract. No sockets, application data or clocks.
#![forbid(unsafe_code)]
pub mod asset;
pub mod wire;
pub mod state_delta;
pub mod visibility;
