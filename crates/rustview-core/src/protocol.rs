//! Versioned wire messages shared by the desktop client and blind relay.

use std::ops::{BitOr, BitOrAssign};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

/// Current RustView wire protocol version.
pub const PROTOCOL_VERSION: u16 = 1;
/// Binary route identifier length.
pub const ROUTE_ID_LEN: usize = 10;
/// Route identifier used by the relay. It is public and is not an authentication secret.
pub type RouteId = [u8; ROUTE_ID_LEN];

/// Largest accepted JPEG frame after reassembly.
pub const MAX_JPEG_FRAME_SIZE: usize = 8 * 1024 * 1024;
/// Largest JPEG data chunk carried by one peer message.
pub const MAX_JPEG_CHUNK_SIZE: usize = 56 * 1024;
/// Largest UTF-8 byte length accepted for a peer display name.
pub const MAX_VIEWER_NAME_LEN: usize = 64;
/// Largest accepted capture dimension on either axis.
pub const MAX_CAPTURE_DIMENSION: u32 = 8_192;
/// Maximum number of chunks advertised for one JPEG frame.
pub const MAX_JPEG_CHUNKS: u16 = 4_096;

/// Client-to-relay control messages. These messages never contain the pairing secret.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelayRequest {
    Register { version: u16, route_id: RouteId },
    Claim { version: u16, route_id: RouteId },
    Ping { nonce: u64 },
}

impl RelayRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::Register { version, route_id } | Self::Claim { version, route_id } => {
                validate_version(*version)?;
                validate_route_id(route_id)
            }
            Self::Ping { .. } => Ok(()),
        }
    }
}

/// Relay-to-client control messages.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelayResponse {
    Registered { ttl_secs: u32 },
    ClaimAccepted,
    Pong { nonce: u64 },
    Error { code: RelayErrorCode },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelayErrorCode {
    Unavailable,
    Busy,
    RateLimited,
    VersionMismatch,
    InvalidRequest,
    Internal,
}

/// A validated set of permissions granted by the local host.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct PermissionSet(u8);

impl PermissionSet {
    pub const NONE: Self = Self(0);
    pub const VIEW_SCREEN: Self = Self(1 << 0);
    pub const CONTROL_POINTER: Self = Self(1 << 1);
    pub const CONTROL_KEYBOARD: Self = Self(1 << 2);
    pub const READ_CLIPBOARD: Self = Self(1 << 3);
    pub const WRITE_CLIPBOARD: Self = Self(1 << 4);
    pub const VIEW_ONLY: Self = Self::VIEW_SCREEN;
    pub const VIEW_AND_CONTROL: Self =
        Self(Self::VIEW_SCREEN.0 | Self::CONTROL_POINTER.0 | Self::CONTROL_KEYBOARD.0);
    pub const ALL: Self = Self(
        Self::VIEW_SCREEN.0
            | Self::CONTROL_POINTER.0
            | Self::CONTROL_KEYBOARD.0
            | Self::READ_CLIPBOARD.0
            | Self::WRITE_CLIPBOARD.0,
    );

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    #[must_use]
    pub const fn from_bits(bits: u8) -> Option<Self> {
        if bits & !Self::ALL.0 == 0 {
            Some(Self(bits))
        } else {
            None
        }
    }

    #[must_use]
    pub const fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    #[must_use]
    pub const fn is_subset_of(self, other: Self) -> bool {
        other.contains(self)
    }

    #[must_use]
    pub const fn view_only() -> Self {
        Self::VIEW_ONLY
    }

    #[must_use]
    pub const fn with_remote_control(self) -> Self {
        Self(self.0 | Self::CONTROL_POINTER.0 | Self::CONTROL_KEYBOARD.0)
    }

    #[must_use]
    pub const fn can_control(self) -> bool {
        self.contains(Self::CONTROL_POINTER) || self.contains(Self::CONTROL_KEYBOARD)
    }
}

impl BitOr for PermissionSet {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for PermissionSet {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl Serialize for PermissionSet {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> Deserialize<'de> for PermissionSet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let bits = u8::deserialize(deserializer)?;
        Self::from_bits(bits).ok_or_else(|| de::Error::custom("unknown permission bits"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRequest {
    pub request_id: [u8; 16],
    pub viewer_name: String,
    pub requested_permissions: PermissionSet,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGrant {
    pub request_id: [u8; 16],
    pub session_id: [u8; 16],
    pub grant_epoch: u32,
    pub granted_permissions: PermissionSet,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JpegFrameStart {
    pub frame_id: u64,
    pub width: u32,
    pub height: u32,
    pub total_len: u32,
    pub chunk_count: u16,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JpegFrameChunk {
    pub frame_id: u64,
    pub chunk_index: u16,
    pub data: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ButtonState {
    Pressed,
    Released,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Cross-platform remote input. Absolute pointer coordinates are normalized to `u16`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputEvent {
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: MouseButton,
        state: ButtonState,
    },
    Scroll {
        delta_x: i16,
        delta_y: i16,
    },
    /// USB HID usage code and transition.
    Key {
        usage: u16,
        state: ButtonState,
    },
}

impl InputEvent {
    #[must_use]
    pub const fn required_permission(self) -> PermissionSet {
        match self {
            Self::MouseMove { .. } | Self::MouseButton { .. } | Self::Scroll { .. } => {
                PermissionSet::CONTROL_POINTER
            }
            Self::Key { .. } => PermissionSet::CONTROL_KEYBOARD,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputMessage {
    pub session_id: [u8; 16],
    pub grant_epoch: u32,
    pub sequence: u64,
    pub event: InputEvent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DisconnectReason {
    Normal,
    Rejected,
    Revoked,
    Timeout,
    ProtocolError,
}

/// End-to-end encrypted messages exchanged by host and viewer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerMessage {
    SessionRequest(SessionRequest),
    SessionGrant(SessionGrant),
    FrameStart(JpegFrameStart),
    FrameChunk(JpegFrameChunk),
    Input(InputMessage),
    Ping { nonce: u64 },
    Pong { nonce: u64 },
    Disconnect { reason: DisconnectReason },
}

impl PeerMessage {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        match self {
            Self::SessionRequest(request) => request.validate(),
            Self::SessionGrant(grant) => grant.validate(),
            Self::FrameStart(frame) => frame.validate(),
            Self::FrameChunk(chunk) => chunk.validate(),
            Self::Input(input) => input.validate(),
            Self::Ping { .. } | Self::Pong { .. } | Self::Disconnect { .. } => Ok(()),
        }
    }

    #[must_use]
    pub const fn required_permission(&self) -> Option<PermissionSet> {
        match self {
            Self::FrameStart(_) | Self::FrameChunk(_) => Some(PermissionSet::VIEW_SCREEN),
            Self::Input(input) => Some(input.event.required_permission()),
            _ => None,
        }
    }
}

impl SessionRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.request_id == [0; 16] {
            return Err(ProtocolError::ZeroIdentifier("request_id"));
        }
        if self.viewer_name.is_empty() || self.viewer_name.len() > MAX_VIEWER_NAME_LEN {
            return Err(ProtocolError::InvalidViewerName);
        }
        if !self
            .viewer_name
            .bytes()
            .all(|byte| byte == b' ' || byte.is_ascii_graphic())
        {
            return Err(ProtocolError::InvalidViewerName);
        }
        if self.requested_permissions.is_empty() {
            return Err(ProtocolError::EmptyPermissions);
        }
        Ok(())
    }
}

impl SessionGrant {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.request_id == [0; 16] {
            return Err(ProtocolError::ZeroIdentifier("request_id"));
        }
        if self.session_id == [0; 16] {
            return Err(ProtocolError::ZeroIdentifier("session_id"));
        }
        if self.grant_epoch == 0 {
            return Err(ProtocolError::InvalidGrantEpoch);
        }
        if !self
            .granted_permissions
            .contains(PermissionSet::VIEW_SCREEN)
        {
            return Err(ProtocolError::ViewPermissionRequired);
        }
        Ok(())
    }
}

impl JpegFrameStart {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.width == 0
            || self.height == 0
            || self.width > MAX_CAPTURE_DIMENSION
            || self.height > MAX_CAPTURE_DIMENSION
        {
            return Err(ProtocolError::InvalidDimensions);
        }
        if self.total_len == 0 || self.total_len as usize > MAX_JPEG_FRAME_SIZE {
            return Err(ProtocolError::InvalidJpegLength);
        }
        if self.chunk_count == 0 || self.chunk_count > MAX_JPEG_CHUNKS {
            return Err(ProtocolError::InvalidChunkCount);
        }
        Ok(())
    }
}

impl JpegFrameChunk {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.data.is_empty() || self.data.len() > MAX_JPEG_CHUNK_SIZE {
            return Err(ProtocolError::InvalidChunkLength);
        }
        Ok(())
    }
}

impl InputMessage {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.session_id == [0; 16] {
            return Err(ProtocolError::ZeroIdentifier("session_id"));
        }
        if self.grant_epoch == 0 {
            return Err(ProtocolError::InvalidGrantEpoch);
        }
        if matches!(self.event, InputEvent::Key { usage: 0, .. }) {
            return Err(ProtocolError::InvalidKeyUsage);
        }
        Ok(())
    }
}

fn validate_version(version: u16) -> Result<(), ProtocolError> {
    if version == PROTOCOL_VERSION {
        Ok(())
    } else {
        Err(ProtocolError::UnsupportedVersion(version))
    }
}

fn validate_route_id(route_id: &RouteId) -> Result<(), ProtocolError> {
    if route_id.iter().all(|byte| *byte == 0) {
        Err(ProtocolError::ZeroIdentifier("route_id"))
    } else {
        Ok(())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u16),
    #[error("{0} must not be all zeroes")]
    ZeroIdentifier(&'static str),
    #[error("viewer name is empty, too long, or contains unsafe characters")]
    InvalidViewerName,
    #[error("permission set must not be empty")]
    EmptyPermissions,
    #[error("screen viewing permission is required")]
    ViewPermissionRequired,
    #[error("grant epoch must be non-zero")]
    InvalidGrantEpoch,
    #[error("capture dimensions are invalid")]
    InvalidDimensions,
    #[error("JPEG frame length is invalid")]
    InvalidJpegLength,
    #[error("JPEG chunk count is invalid")]
    InvalidChunkCount,
    #[error("JPEG chunk length is invalid")]
    InvalidChunkLength,
    #[error("USB HID usage zero is invalid")]
    InvalidKeyUsage,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_permission_bits_are_rejected() {
        let bytes = postcard::to_stdvec(&0x80_u8).expect("serialize bits");
        assert!(postcard::from_bytes::<PermissionSet>(&bytes).is_err());
    }

    #[test]
    fn input_permissions_are_directional() {
        let pointer = InputEvent::MouseMove { x: 1, y: 2 };
        let key = InputEvent::Key {
            usage: 4,
            state: ButtonState::Pressed,
        };
        assert_eq!(
            pointer.required_permission(),
            PermissionSet::CONTROL_POINTER
        );
        assert_eq!(key.required_permission(), PermissionSet::CONTROL_KEYBOARD);
    }
}
