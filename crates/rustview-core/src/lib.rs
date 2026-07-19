//! Shared protocol, framing, encryption, and authorization primitives for RustView.

pub mod device;
pub mod framing;
pub mod invite;
pub mod protocol;
pub mod secure;
pub mod session;

pub use device::{AccessPassword, DeviceId};
pub use invite::Invitation;
pub use secure::{Role, SecureChannel};
