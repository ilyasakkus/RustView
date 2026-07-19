//! Parsing and generation of one-time RustView invitations.

use std::{fmt, str::FromStr};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use data_encoding::BASE32_NOPAD;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::protocol::{ROUTE_ID_LEN, RouteId};

pub const INVITATION_VERSION: &str = "RV1";
pub const PAIRING_SECRET_LEN: usize = 32;
pub const ROUTE_CODE_LEN: usize = 16;
pub const SECRET_CODE_LEN: usize = 43;

/// A one-time bearer invitation. Its debug representation always redacts the secret.
pub struct Invitation {
    route_id: RouteId,
    secret: Zeroizing<[u8; PAIRING_SECRET_LEN]>,
}

impl Invitation {
    pub fn generate() -> Result<Self, InvitationError> {
        let mut route_id = [0_u8; ROUTE_ID_LEN];
        let mut secret = [0_u8; PAIRING_SECRET_LEN];
        getrandom::fill(&mut route_id).map_err(|_| InvitationError::RandomnessUnavailable)?;
        getrandom::fill(&mut secret).map_err(|_| InvitationError::RandomnessUnavailable)?;
        Self::new(route_id, secret)
    }

    pub fn new(
        route_id: RouteId,
        secret: [u8; PAIRING_SECRET_LEN],
    ) -> Result<Self, InvitationError> {
        if route_id.iter().all(|byte| *byte == 0) {
            return Err(InvitationError::ZeroRouteId);
        }
        if secret.iter().all(|byte| *byte == 0) {
            return Err(InvitationError::ZeroSecret);
        }
        Ok(Self {
            route_id,
            secret: Zeroizing::new(secret),
        })
    }

    #[must_use]
    pub const fn route_id(&self) -> RouteId {
        self.route_id
    }

    #[must_use]
    pub fn route_code(&self) -> String {
        BASE32_NOPAD.encode(&self.route_id)
    }

    /// Intentionally exposes the PSK to the secure-channel builder.
    #[must_use]
    pub fn secret(&self) -> &[u8; PAIRING_SECRET_LEN] {
        &self.secret
    }

    /// Returns the canonical invitation text intended for explicit copy/share UI.
    #[must_use]
    pub fn encode(&self) -> String {
        format!(
            "{INVITATION_VERSION}.{}.{}",
            self.route_code(),
            URL_SAFE_NO_PAD.encode(self.secret.as_ref())
        )
    }

    pub(crate) fn noise_prologue(&self) -> Vec<u8> {
        let mut prologue = Vec::with_capacity(16 + self.route_id.len());
        prologue.extend_from_slice(b"RustView-E2EE\0");
        prologue.extend_from_slice(&1_u16.to_be_bytes());
        prologue.extend_from_slice(&self.route_id);
        prologue
    }
}

impl Clone for Invitation {
    fn clone(&self) -> Self {
        Self {
            route_id: self.route_id,
            secret: self.secret.clone(),
        }
    }
}

impl fmt::Debug for Invitation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Invitation")
            .field("route_code", &self.route_code())
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Display for Invitation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.encode())
    }
}

impl FromStr for Invitation {
    type Err = InvitationError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.trim() != value {
            return Err(InvitationError::InvalidFormat);
        }
        let mut parts = value.split('.');
        let version = parts.next().ok_or(InvitationError::InvalidFormat)?;
        let route_code = parts.next().ok_or(InvitationError::InvalidFormat)?;
        let secret_code = parts.next().ok_or(InvitationError::InvalidFormat)?;
        if parts.next().is_some() {
            return Err(InvitationError::InvalidFormat);
        }
        if version != INVITATION_VERSION {
            return Err(InvitationError::UnsupportedVersion);
        }
        if route_code.len() != ROUTE_CODE_LEN || secret_code.len() != SECRET_CODE_LEN {
            return Err(InvitationError::InvalidFormat);
        }

        let route = BASE32_NOPAD
            .decode(route_code.as_bytes())
            .map_err(|_| InvitationError::InvalidRouteEncoding)?;
        let route_id: RouteId = route
            .try_into()
            .map_err(|_| InvitationError::InvalidRouteEncoding)?;
        if BASE32_NOPAD.encode(&route_id) != route_code {
            return Err(InvitationError::NonCanonicalEncoding);
        }

        let secret = URL_SAFE_NO_PAD
            .decode(secret_code)
            .map_err(|_| InvitationError::InvalidSecretEncoding)?;
        let secret: [u8; PAIRING_SECRET_LEN] = secret
            .try_into()
            .map_err(|_| InvitationError::InvalidSecretEncoding)?;
        if URL_SAFE_NO_PAD.encode(secret) != secret_code {
            return Err(InvitationError::NonCanonicalEncoding);
        }
        Self::new(route_id, secret)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum InvitationError {
    #[error("operating-system randomness is unavailable")]
    RandomnessUnavailable,
    #[error("invitation must be RV1.<BASE32_ROUTE>.<BASE64URL_SECRET>")]
    InvalidFormat,
    #[error("unsupported invitation version")]
    UnsupportedVersion,
    #[error("route identifier is not valid Base32")]
    InvalidRouteEncoding,
    #[error("pairing secret is not valid unpadded Base64URL")]
    InvalidSecretEncoding,
    #[error("invitation encoding is not canonical")]
    NonCanonicalEncoding,
    #[error("route identifier must not be all zeroes")]
    ZeroRouteId,
    #[error("pairing secret must not be all zeroes")]
    ZeroSecret,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Invitation {
        Invitation::new([0x42; ROUTE_ID_LEN], [0x24; PAIRING_SECRET_LEN]).expect("valid fixture")
    }

    #[test]
    fn canonical_invitation_round_trips() {
        let invitation = fixture();
        let encoded = invitation.encode();
        assert!(encoded.starts_with("RV1.IJBEEQSCIJBEEQSC."));
        assert_eq!(encoded.len(), 5 + ROUTE_CODE_LEN + SECRET_CODE_LEN);

        let decoded: Invitation = encoded.parse().expect("parse invitation");
        assert_eq!(decoded.route_id(), invitation.route_id());
        assert_eq!(decoded.secret(), invitation.secret());
        assert_eq!(decoded.encode(), encoded);
    }

    #[test]
    fn debug_always_redacts_secret() {
        let invitation = fixture();
        let debug = format!("{invitation:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&URL_SAFE_NO_PAD.encode(invitation.secret())));
    }

    #[test]
    fn rejects_whitespace_padding_lowercase_and_wrong_lengths() {
        let canonical = fixture().encode();
        assert!(format!(" {canonical}").parse::<Invitation>().is_err());
        assert!(
            canonical
                .to_ascii_lowercase()
                .parse::<Invitation>()
                .is_err()
        );
        assert!(format!("{canonical}=").parse::<Invitation>().is_err());
        assert!("RV1.AAAA.AAAA".parse::<Invitation>().is_err());
    }

    #[test]
    fn rejects_zero_material() {
        assert!(Invitation::new([0; ROUTE_ID_LEN], [1; PAIRING_SECRET_LEN]).is_err());
        assert!(Invitation::new([1; ROUTE_ID_LEN], [0; PAIRING_SECRET_LEN]).is_err());
    }
}
