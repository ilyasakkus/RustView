//! End-to-end encrypted blocking channel over a relayed TCP byte stream.

use std::{
    fmt, io,
    net::{Shutdown, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use snow::{Builder, HandshakeState, StatelessTransportState, params::NoiseParams};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    framing::{
        FramingError, MAX_FRAME_SIZE, decode_message, encode_message, read_frame, write_frame,
    },
    invite::Invitation,
    protocol::{PeerMessage, ProtocolError},
};

pub const NOISE_PATTERN: &str = "Noise_XXpsk0_25519_ChaChaPoly_BLAKE2s";
pub const STATIC_KEY_LEN: usize = 32;
pub const HANDSHAKE_MAX_FRAME_SIZE: usize = 1_024;

/// Viewer initiates the Noise handshake; host responds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Viewer,
    Host,
}

/// Noise static keypair. Persist this securely to retain a stable peer fingerprint.
pub struct StaticKeypair {
    private: Zeroizing<[u8; STATIC_KEY_LEN]>,
    public: [u8; STATIC_KEY_LEN],
}

impl StaticKeypair {
    pub fn generate() -> Result<Self, SecureError> {
        let params: NoiseParams = NOISE_PATTERN
            .parse()
            .map_err(|error| SecureError::NoisePattern(format!("{error:?}")))?;
        let keypair = Builder::new(params).generate_keypair()?;
        let private = keypair
            .private
            .try_into()
            .map_err(|_| SecureError::InvalidStaticKey)?;
        let public = keypair
            .public
            .try_into()
            .map_err(|_| SecureError::InvalidStaticKey)?;
        Self::from_parts(private, public)
    }

    pub fn from_parts(
        private: [u8; STATIC_KEY_LEN],
        public: [u8; STATIC_KEY_LEN],
    ) -> Result<Self, SecureError> {
        if private.iter().all(|byte| *byte == 0) || public.iter().all(|byte| *byte == 0) {
            return Err(SecureError::InvalidStaticKey);
        }
        Ok(Self {
            private: Zeroizing::new(private),
            public,
        })
    }

    #[must_use]
    pub fn private(&self) -> &[u8; STATIC_KEY_LEN] {
        &self.private
    }

    #[must_use]
    pub const fn public(&self) -> [u8; STATIC_KEY_LEN] {
        self.public
    }
}

impl fmt::Debug for StaticKeypair {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StaticKeypair")
            .field("private", &"[REDACTED]")
            .field("public", &self.public)
            .finish()
    }
}

/// Cloneable, thread-safe channel with independently locked blocking send/receive paths.
#[derive(Clone)]
pub struct SecureChannel {
    inner: Arc<SecureChannelInner>,
}

struct SecureChannelInner {
    transport: StatelessTransportState,
    reader: Mutex<TcpStream>,
    writer: Mutex<TcpStream>,
    /// A lock-free handle used to interrupt a thread blocked in `recv`.
    shutdown_handle: TcpStream,
    send_nonce: AtomicU64,
    receive_nonce: AtomicU64,
    closed: AtomicBool,
    peer_static: [u8; STATIC_KEY_LEN],
    handshake_hash: [u8; 32],
}

impl SecureChannel {
    /// Establishes a session using a fresh per-connection static keypair.
    /// Prefer [`Self::establish_with_key`] when the application persists device identity.
    pub fn establish(
        stream: TcpStream,
        role: Role,
        invitation: &Invitation,
    ) -> Result<Self, SecureError> {
        let local_key = StaticKeypair::generate()?;
        Self::establish_with_key(stream, role, invitation, &local_key)
    }

    pub fn establish_with_key(
        mut stream: TcpStream,
        role: Role,
        invitation: &Invitation,
        local_key: &StaticKeypair,
    ) -> Result<Self, SecureError> {
        let mut handshake = build_handshake(role, invitation, local_key)?;
        run_handshake(&mut stream, role, &mut handshake)?;

        let peer_static: [u8; STATIC_KEY_LEN] = handshake
            .get_remote_static()
            .ok_or(SecureError::MissingPeerStatic)?
            .try_into()
            .map_err(|_| SecureError::InvalidStaticKey)?;
        let handshake_hash: [u8; 32] = handshake
            .get_handshake_hash()
            .try_into()
            .map_err(|_| SecureError::InvalidHandshakeHash)?;
        let transport = handshake.into_stateless_transport_mode()?;

        let reader = stream.try_clone()?;
        let writer = stream.try_clone()?;
        Ok(Self {
            inner: Arc::new(SecureChannelInner {
                transport,
                reader: Mutex::new(reader),
                writer: Mutex::new(writer),
                shutdown_handle: stream,
                send_nonce: AtomicU64::new(0),
                receive_nonce: AtomicU64::new(0),
                closed: AtomicBool::new(false),
                peer_static,
                handshake_hash,
            }),
        })
    }

    pub fn send(&self, message: &PeerMessage) -> Result<(), SecureError> {
        if self.is_closed() {
            return Err(SecureError::Closed);
        }
        message.validate()?;
        let plaintext = encode_message(message)?;
        let mut ciphertext = vec![0_u8; plaintext.len() + 16];
        let mut writer = self
            .inner
            .writer
            .lock()
            .map_err(|_| SecureError::Poisoned("writer"))?;
        if self.is_closed() {
            return Err(SecureError::Closed);
        }

        let nonce = self.inner.send_nonce.load(Ordering::Acquire);
        let next_nonce = nonce.checked_add(1).ok_or(SecureError::NonceExhausted)?;
        let length = self
            .inner
            .transport
            .write_message(nonce, &plaintext, &mut ciphertext)?;
        if length > MAX_FRAME_SIZE {
            return Err(SecureError::CiphertextTooLarge(length));
        }
        if let Err(error) = write_frame(&mut *writer, &ciphertext[..length]) {
            let _ = self.shutdown();
            return Err(error.into());
        }
        self.inner.send_nonce.store(next_nonce, Ordering::Release);
        Ok(())
    }

    pub fn recv(&self) -> Result<PeerMessage, SecureError> {
        if self.is_closed() {
            return Err(SecureError::Closed);
        }
        let mut reader = self
            .inner
            .reader
            .lock()
            .map_err(|_| SecureError::Poisoned("reader"))?;
        let ciphertext = match read_frame(&mut *reader) {
            Ok(frame) => frame,
            Err(error) => {
                let _ = self.shutdown();
                return Err(error.into());
            }
        };
        let nonce = self.inner.receive_nonce.load(Ordering::Acquire);
        let next_nonce = nonce.checked_add(1).ok_or(SecureError::NonceExhausted)?;
        let mut plaintext = vec![0_u8; ciphertext.len()];
        let length = match self
            .inner
            .transport
            .read_message(nonce, &ciphertext, &mut plaintext)
        {
            Ok(length) => length,
            Err(error) => {
                let _ = self.shutdown();
                return Err(error.into());
            }
        };
        self.inner
            .receive_nonce
            .store(next_nonce, Ordering::Release);
        let message: PeerMessage = match decode_message(&plaintext[..length]) {
            Ok(message) => message,
            Err(error) => {
                let _ = self.shutdown();
                return Err(error.into());
            }
        };
        if let Err(error) = message.validate() {
            let _ = self.shutdown();
            return Err(error.into());
        }
        Ok(message)
    }

    /// Sets the timeout used by future blocking receive operations on every clone.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<(), SecureError> {
        if self.is_closed() {
            return Err(SecureError::Closed);
        }
        let reader = self
            .inner
            .reader
            .lock()
            .map_err(|_| SecureError::Poisoned("reader"))?;
        reader.set_read_timeout(timeout)?;
        Ok(())
    }

    /// Sets the timeout used by future blocking send operations on every clone.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> Result<(), SecureError> {
        if self.is_closed() {
            return Err(SecureError::Closed);
        }
        let writer = self
            .inner
            .writer
            .lock()
            .map_err(|_| SecureError::Poisoned("writer"))?;
        writer.set_write_timeout(timeout)?;
        Ok(())
    }

    /// Interrupts blocked reads and prevents future sends/receives on every clone.
    pub fn shutdown(&self) -> io::Result<()> {
        self.inner.closed.store(true, Ordering::Release);
        self.inner.shutdown_handle.shutdown(Shutdown::Both)
    }

    /// Alias for [`Self::shutdown`].
    pub fn close(&self) -> io::Result<()> {
        self.shutdown()
    }

    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn peer_static(&self) -> [u8; STATIC_KEY_LEN] {
        self.inner.peer_static
    }

    #[must_use]
    pub fn handshake_hash(&self) -> [u8; 32] {
        self.inner.handshake_hash
    }
}

impl fmt::Debug for SecureChannel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SecureChannel")
            .field("closed", &self.is_closed())
            .field("peer_static", &self.peer_static())
            .finish_non_exhaustive()
    }
}

impl Drop for SecureChannelInner {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        let _ = self.shutdown_handle.shutdown(Shutdown::Both);
    }
}

fn build_handshake(
    role: Role,
    invitation: &Invitation,
    local_key: &StaticKeypair,
) -> Result<HandshakeState, SecureError> {
    let params: NoiseParams = NOISE_PATTERN
        .parse()
        .map_err(|error| SecureError::NoisePattern(format!("{error:?}")))?;
    let prologue = invitation.noise_prologue();
    let builder = Builder::new(params)
        .local_private_key(local_key.private())?
        .psk(0, invitation.secret())?
        .prologue(&prologue)?;
    match role {
        Role::Viewer => Ok(builder.build_initiator()?),
        Role::Host => Ok(builder.build_responder()?),
    }
}

fn run_handshake(
    stream: &mut TcpStream,
    role: Role,
    handshake: &mut HandshakeState,
) -> Result<(), SecureError> {
    match role {
        Role::Viewer => {
            write_handshake_message(stream, handshake)?;
            read_handshake_message(stream, handshake)?;
            write_handshake_message(stream, handshake)?;
        }
        Role::Host => {
            read_handshake_message(stream, handshake)?;
            write_handshake_message(stream, handshake)?;
            read_handshake_message(stream, handshake)?;
        }
    }
    if !handshake.is_handshake_finished() {
        return Err(SecureError::IncompleteHandshake);
    }
    Ok(())
}

fn write_handshake_message(
    stream: &mut TcpStream,
    handshake: &mut HandshakeState,
) -> Result<(), SecureError> {
    let mut output = [0_u8; HANDSHAKE_MAX_FRAME_SIZE];
    let length = handshake.write_message(&[], &mut output)?;
    write_frame(stream, &output[..length])?;
    Ok(())
}

fn read_handshake_message(
    stream: &mut TcpStream,
    handshake: &mut HandshakeState,
) -> Result<(), SecureError> {
    let input = read_frame(stream)?;
    if input.len() > HANDSHAKE_MAX_FRAME_SIZE {
        return Err(SecureError::HandshakeFrameTooLarge(input.len()));
    }
    let mut payload = [0_u8; HANDSHAKE_MAX_FRAME_SIZE];
    let payload_length = handshake.read_message(&input, &mut payload)?;
    if payload_length != 0 {
        return Err(SecureError::UnexpectedHandshakePayload);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum SecureError {
    #[error("I/O error in secure channel")]
    Io(#[from] io::Error),
    #[error("invalid Noise pattern: {0}")]
    NoisePattern(String),
    #[error("Noise protocol error")]
    Noise(#[from] snow::Error),
    #[error("framing error")]
    Framing(#[from] FramingError),
    #[error("invalid peer protocol message")]
    Protocol(#[from] ProtocolError),
    #[error("static key must contain a 32-byte non-zero private and public key")]
    InvalidStaticKey,
    #[error("Noise handshake did not expose a peer static key")]
    MissingPeerStatic,
    #[error("Noise handshake hash is not 32 bytes")]
    InvalidHandshakeHash,
    #[error("Noise handshake did not finish")]
    IncompleteHandshake,
    #[error("handshake frame is {0} bytes; maximum is {HANDSHAKE_MAX_FRAME_SIZE}")]
    HandshakeFrameTooLarge(usize),
    #[error("handshake messages must not contain application payloads")]
    UnexpectedHandshakePayload,
    #[error("ciphertext is {0} bytes; maximum is {MAX_FRAME_SIZE}")]
    CiphertextTooLarge(usize),
    #[error("secure channel nonce exhausted")]
    NonceExhausted,
    #[error("secure channel is closed")]
    Closed,
    #[error("secure channel {0} lock is poisoned")]
    Poisoned(&'static str),
}

#[cfg(test)]
mod tests {
    use std::{net::TcpListener, thread, time::Duration};

    use crate::invite::PAIRING_SECRET_LEN;
    use crate::protocol::DisconnectReason;

    use super::*;

    #[test]
    fn channel_is_bidirectional_and_peer_keys_match() {
        let invitation = Invitation::new([7; 10], [9; PAIRING_SECRET_LEN]).expect("invitation");
        let host_key = StaticKeypair::generate().expect("host key");
        let host_public = host_key.public();
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("address");
        let host_invitation = invitation.clone();
        let host = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let channel =
                SecureChannel::establish_with_key(stream, Role::Host, &host_invitation, &host_key)
                    .expect("host handshake");
            let incoming = channel.recv().expect("host receive");
            assert_eq!(incoming, PeerMessage::Ping { nonce: 42 });
            channel
                .send(&PeerMessage::Pong { nonce: 42 })
                .expect("host send");
            channel.peer_static()
        });

        let viewer_key = StaticKeypair::generate().expect("viewer key");
        let viewer_public = viewer_key.public();
        let stream = TcpStream::connect(address).expect("connect");
        let viewer =
            SecureChannel::establish_with_key(stream, Role::Viewer, &invitation, &viewer_key)
                .expect("viewer handshake");
        viewer
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("set read timeout");
        viewer
            .set_write_timeout(Some(Duration::from_secs(2)))
            .expect("set write timeout");
        viewer
            .send(&PeerMessage::Ping { nonce: 42 })
            .expect("viewer send");
        assert_eq!(
            viewer.recv().expect("viewer receive"),
            PeerMessage::Pong { nonce: 42 }
        );
        assert_eq!(viewer.peer_static(), host_public);
        assert_eq!(host.join().expect("host thread"), viewer_public);

        // TCP may accept one final buffered write even after the peer has exited,
        // so shutdown state is the deterministic assertion here.
        let _ = viewer.send(&PeerMessage::Disconnect {
            reason: DisconnectReason::Normal,
        });
        let _ = viewer.shutdown();
        assert!(viewer.is_closed());
        assert!(matches!(
            viewer.set_read_timeout(Some(Duration::from_secs(1))),
            Err(SecureError::Closed)
        ));
        assert!(matches!(
            viewer.set_write_timeout(Some(Duration::from_secs(1))),
            Err(SecureError::Closed)
        ));
    }

    #[test]
    fn wrong_pairing_secret_fails_the_handshake() {
        let host_invitation = Invitation::new([7; 10], [9; PAIRING_SECRET_LEN]).unwrap();
        let viewer_invitation = Invitation::new([7; 10], [8; PAIRING_SECRET_LEN]).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("address");
        let host = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            SecureChannel::establish(stream, Role::Host, &host_invitation).is_err()
        });

        let stream = TcpStream::connect(address).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        assert!(SecureChannel::establish(stream, Role::Viewer, &viewer_invitation).is_err());
        assert!(host.join().expect("host thread"));
    }
}
