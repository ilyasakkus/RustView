//! Stable device identifiers and ephemeral access passwords.

use std::{fmt, str::FromStr};

use blake2::{Blake2s256, Digest as _};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    Invitation,
    invite::PAIRING_SECRET_LEN,
    protocol::{ROUTE_ID_LEN, RouteId},
};

const DEVICE_ID_DIGITS: usize = 9;
const DEVICE_ID_RANGE: u32 = 999_999_999;
const PASSWORD_LEN: usize = 16;
const PASSWORD_ALPHABET: &[u8; 32] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
const PASSWORD_ENTROPY_BYTES: usize = 10;
const ROUTE_DOMAIN: &[u8] = b"RustView device route id\0v1\0";
const ACCESS_ROUTE_DOMAIN: &[u8] = b"RustView password-protected route id\0v1\0";
const PAIRING_DOMAIN: &[u8] = b"RustView pairing secret\0v1\0";

/// A stable, non-zero nine-digit identifier for one RustView installation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DeviceId(u32);

impl DeviceId {
    /// Generates an unbiased identifier in the inclusive range `000 000 001` to
    /// `999 999 999` using the operating system's random-number generator.
    pub fn generate() -> Result<Self, DeviceIdentityError> {
        // Work in u64 so the upper-exclusive rejection boundary can represent
        // the complete 2^32 sample space.
        let sample_space = u64::from(u32::MAX) + 1;
        let range = u64::from(DEVICE_ID_RANGE);
        let unbiased_boundary = (sample_space / range) * range;

        loop {
            let mut bytes = [0_u8; size_of::<u32>()];
            getrandom::fill(&mut bytes).map_err(|_| DeviceIdentityError::RandomnessUnavailable)?;
            let candidate = u64::from(u32::from_be_bytes(bytes));
            if candidate < unbiased_boundary {
                let value = u32::try_from(candidate % range + 1)
                    .expect("device identifier always fits in u32");
                return Ok(Self(value));
            }
        }
    }

    /// Returns the normalized, zero-padded nine ASCII digits.
    #[must_use]
    pub fn canonical_digits(self) -> String {
        format!("{:0DEVICE_ID_DIGITS$}", self.0)
    }

    /// Derives the stable relay route for this installation.
    #[must_use]
    pub fn route_id(self) -> RouteId {
        let digest = domain_hash(ROUTE_DOMAIN, &[self.canonical_digits().as_bytes()]);
        let mut route_id = [0_u8; ROUTE_ID_LEN];
        route_id.copy_from_slice(&digest[..ROUTE_ID_LEN]);

        // Invitation deliberately rejects an all-zero route. Preserve a total,
        // deterministic API even for that cryptographically negligible digest.
        if route_id.iter().all(|byte| *byte == 0) {
            route_id[ROUTE_ID_LEN - 1] = 1;
        }
        route_id
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.canonical_digits();
        write!(
            formatter,
            "{} {} {}",
            &digits[..3],
            &digits[3..6],
            &digits[6..]
        )
    }
}

impl FromStr for DeviceId {
    type Err = DeviceIdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized: String = value
            .chars()
            .filter(|character| !matches!(character, ' ' | '-'))
            .collect();
        if normalized.len() != DEVICE_ID_DIGITS
            || !normalized.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(DeviceIdentityError::InvalidDeviceId);
        }

        let numeric = normalized
            .parse::<u32>()
            .map_err(|_| DeviceIdentityError::InvalidDeviceId)?;
        if numeric == 0 || numeric > DEVICE_ID_RANGE {
            return Err(DeviceIdentityError::InvalidDeviceId);
        }
        Ok(Self(numeric))
    }
}

/// A per-launch, 80-bit access password. Its debug output is always redacted.
pub struct AccessPassword(Zeroizing<String>);

impl AccessPassword {
    /// Generates 16 symbols from an unambiguous 32-character alphabet.
    pub fn generate() -> Result<Self, DeviceIdentityError> {
        let mut entropy = Zeroizing::new([0_u8; PASSWORD_ENTROPY_BYTES]);
        getrandom::fill(entropy.as_mut())
            .map_err(|_| DeviceIdentityError::RandomnessUnavailable)?;

        let mut password = String::with_capacity(PASSWORD_LEN);
        let mut buffer = 0_u16;
        let mut buffered_bits = 0_u8;
        for byte in entropy.iter().copied() {
            buffer = (buffer << 8) | u16::from(byte);
            buffered_bits += 8;
            while buffered_bits >= 5 {
                buffered_bits -= 5;
                let index = usize::from((buffer >> buffered_bits) & 0x1f);
                password.push(char::from(PASSWORD_ALPHABET[index]));
                buffer &= if buffered_bits == 0 {
                    0
                } else {
                    (1_u16 << buffered_bits) - 1
                };
            }
        }
        debug_assert_eq!(password.len(), PASSWORD_LEN);
        Ok(Self(Zeroizing::new(password)))
    }

    /// Explicitly exposes the password for the local UI.
    #[must_use]
    pub fn expose(&self) -> &str {
        self.0.as_str()
    }

    /// Derives the pairing secret used by the encrypted transport.
    #[must_use]
    pub fn pairing_secret(&self, device_id: &DeviceId) -> [u8; PAIRING_SECRET_LEN] {
        let digits = device_id.canonical_digits();
        let mut secret = domain_hash(
            PAIRING_DOMAIN,
            &[digits.as_bytes(), b"\0", self.expose().as_bytes()],
        );
        if secret.iter().all(|byte| *byte == 0) {
            secret[PAIRING_SECRET_LEN - 1] = 1;
        }
        secret
    }

    /// Derives an unguessable relay route from both normalized credentials.
    ///
    /// This deliberately differs from [`DeviceId::route_id`]: knowing a public
    /// nine-digit ID is not enough to enumerate or claim its relay route.
    #[must_use]
    pub fn route_id(&self, device_id: &DeviceId) -> RouteId {
        let digits = device_id.canonical_digits();
        let digest = domain_hash(
            ACCESS_ROUTE_DOMAIN,
            &[digits.as_bytes(), b"\0", self.expose().as_bytes()],
        );
        let mut route_id = [0_u8; ROUTE_ID_LEN];
        route_id.copy_from_slice(&digest[..ROUTE_ID_LEN]);
        if route_id.iter().all(|byte| *byte == 0) {
            route_id[ROUTE_ID_LEN - 1] = 1;
        }
        route_id
    }

    /// Builds the existing encrypted-channel invitation from an ID and password.
    #[must_use]
    pub fn invitation(&self, device_id: &DeviceId) -> Invitation {
        Invitation::new(self.route_id(device_id), self.pairing_secret(device_id))
            .expect("derived device invitation is always non-zero")
    }
}

impl Clone for AccessPassword {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl fmt::Debug for AccessPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("AccessPassword([REDACTED])")
    }
}

impl FromStr for AccessPassword {
    type Err = DeviceIdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized: String = value
            .chars()
            .filter(|character| !matches!(character, ' ' | '-'))
            .map(|character| character.to_ascii_uppercase())
            .collect();
        if normalized.len() != PASSWORD_LEN
            || !normalized
                .bytes()
                .all(|byte| PASSWORD_ALPHABET.contains(&byte))
        {
            return Err(DeviceIdentityError::InvalidAccessPassword);
        }
        Ok(Self(Zeroizing::new(normalized)))
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum DeviceIdentityError {
    #[error("operating-system randomness is unavailable")]
    RandomnessUnavailable,
    #[error("device ID must contain exactly nine digits and must not be zero")]
    InvalidDeviceId,
    #[error("access password must contain exactly 16 unambiguous characters")]
    InvalidAccessPassword,
}

fn domain_hash(domain: &[u8], components: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Blake2s256::new();
    hasher.update(domain);
    for component in components {
        hasher.update(component);
    }
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_id_parses_and_formats_canonically() {
        let id: DeviceId = "123-456 789".parse().expect("valid device ID");
        assert_eq!(id.canonical_digits(), "123456789");
        assert_eq!(id.to_string(), "123 456 789");

        let leading_zeroes: DeviceId = "000 000 042".parse().expect("valid ID");
        assert_eq!(leading_zeroes.canonical_digits(), "000000042");
        assert_eq!(leading_zeroes.to_string(), "000 000 042");
    }

    #[test]
    fn device_id_rejects_invalid_or_zero_input() {
        for invalid in [
            "000000000",
            "12345678",
            "1234567890",
            "123.456.789",
            "12345678A",
            "１２３４５６７８９",
        ] {
            assert!(invalid.parse::<DeviceId>().is_err(), "accepted {invalid:?}");
        }
    }

    #[test]
    fn generated_device_ids_are_nine_digit_and_nonzero() {
        for _ in 0..128 {
            let id = DeviceId::generate().expect("OS randomness");
            assert_eq!(id.canonical_digits().len(), DEVICE_ID_DIGITS);
            assert_ne!(id.canonical_digits(), "000000000");
        }
    }

    #[test]
    fn route_is_stable_and_bound_to_device_id() {
        let first: DeviceId = "123456789".parse().expect("valid ID");
        let same: DeviceId = "123 456 789".parse().expect("valid ID");
        let other: DeviceId = "123456788".parse().expect("valid ID");
        assert_eq!(first.route_id(), same.route_id());
        assert_ne!(first.route_id(), other.route_id());
        assert_ne!(first.route_id(), [0; ROUTE_ID_LEN]);
    }

    #[test]
    fn password_normalizes_lowercase_and_separators() {
        let password: AccessPassword = "abcd-efgh jkmn-pqrs".parse().expect("valid password");
        assert_eq!(password.expose(), "ABCDEFGHJKMNPQRS");
    }

    #[test]
    fn password_rejects_ambiguous_and_wrong_length_input() {
        for invalid in [
            "ABCDEFGHJKLMNPQ",
            "ABCDEFGHJKLMNPQRS",
            "ABCDEFGHJKLMNPQI",
            "ABCDEFGHJKLMNPQO",
            "ABCDEFGHJKLMNPQ0",
            "ABCDEFGHJKLMNPQ1",
        ] {
            assert!(
                invalid.parse::<AccessPassword>().is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn generated_password_has_full_encoded_entropy_and_redacted_debug() {
        let password = AccessPassword::generate().expect("OS randomness");
        assert_eq!(password.expose().len(), PASSWORD_LEN);
        assert!(
            password
                .expose()
                .bytes()
                .all(|byte| PASSWORD_ALPHABET.contains(&byte))
        );
        let debug = format!("{password:?}");
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(password.expose()));
    }

    #[test]
    fn pairing_secret_is_normalized_and_domain_bound() {
        let first_id: DeviceId = "123456789".parse().expect("valid ID");
        let same_id: DeviceId = "123 456 789".parse().expect("valid ID");
        let other_id: DeviceId = "123456788".parse().expect("valid ID");
        let password: AccessPassword = "abcd-efgh-jkmn-pqrs".parse().expect("valid password");
        let same_password: AccessPassword = "ABCDEFGHJKMNPQRS".parse().expect("valid password");
        let other_password: AccessPassword = "ABCDEFGHJKMNPQRT".parse().expect("valid password");

        assert_eq!(
            password.pairing_secret(&first_id),
            same_password.pairing_secret(&same_id)
        );
        assert_ne!(
            password.pairing_secret(&first_id),
            password.pairing_secret(&other_id)
        );
        assert_ne!(
            password.pairing_secret(&first_id),
            other_password.pairing_secret(&first_id)
        );

        assert_eq!(
            password.route_id(&first_id),
            same_password.route_id(&same_id)
        );
        assert_ne!(password.route_id(&first_id), password.route_id(&other_id));
        assert_ne!(
            password.route_id(&first_id),
            other_password.route_id(&first_id)
        );

        let invitation = password.invitation(&first_id);
        assert_eq!(invitation.route_id(), password.route_id(&first_id));
        assert_ne!(invitation.route_id(), first_id.route_id());
        assert_ne!(
            invitation.route_id().as_slice(),
            &password.pairing_secret(&first_id)[..ROUTE_ID_LEN]
        );
        assert_eq!(invitation.secret(), &password.pairing_secret(&first_id));
    }

    #[test]
    fn relay_route_and_pairing_secret_are_separate_hash_outputs() {
        let id: DeviceId = "731 904 286".parse().expect("valid ID");
        let password: AccessPassword = "7HKN-9RTW-CX3M-ZQ5S".parse().expect("valid password");
        let route = password.route_id(&id);
        let secret = password.pairing_secret(&id);

        assert_ne!(route.as_slice(), &secret[..ROUTE_ID_LEN]);
        assert_eq!(password.invitation(&id).route_id(), route);
    }
}
