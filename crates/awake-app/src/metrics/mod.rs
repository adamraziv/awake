//! System metrics for the remote page. Sampled every 5 seconds on the tick thread and cached in
//! `App`; nothing here is shown locally. Spec section 7.

pub mod battery;
pub mod disk;
mod sysctl;
pub mod uptime;
