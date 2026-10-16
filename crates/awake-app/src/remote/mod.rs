//! Remote control over HTTP on the Tailscale IP. Spec section 6. `auth.rs` decides who may call,
//! `status.rs` shapes `/api/status`, `server.rs` runs the server; the router lives here (Task 6).

pub mod auth;
pub mod status;
