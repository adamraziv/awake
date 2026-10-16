//! Awake menu bar app. Logic lives here so it can be tested; `main.rs` only wires things up.

pub mod app;
pub mod config;
pub mod helper;
pub mod lid;
pub mod logging;
pub mod login_item;
pub mod menu;
pub mod metrics;
pub mod power;
pub mod remote;
pub mod session;
pub mod sleep;
pub mod tailscale;
