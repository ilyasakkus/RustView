# RustView architecture

This document describes RustView's initial MVP architecture, trust boundaries, and
planned network evolution. The current goal is a small vertical slice that is easy
to understand and test and that has secure defaults. This document is not a
commitment to production-ready functionality.

## Design goals

- Use the same Rust codebase on macOS, Windows, and Linux
- Prevent the relay operator from reading screen or input content
- Process no remote input without explicit local approval by the host
- Separate the network, session, platform, and UI layers
- Prevent a slow receiver from causing unbounded memory growth
- Preserve as much of the application protocol as possible when moving from the
  TCP relay to direct QUIC connections in the future

The initial MVP performance target is 720p JPEG at 5–10 FPS. A complete,
low-latency, hardware-accelerated, multi-display remote desktop product is outside
this phase's scope.

## Workspace boundaries

```text
apps/rustview-desktop/
  UI, user interaction, capture/render loop, and session orchestration

crates/rustview-core/
  Identity/password derivation, wire messages, bounded framing, Noise session, and shared types

services/rustview-relay/
  Register/Claim rendezvous and blind forwarding of paired TCP streams
```

Dependencies point from the desktop and relay binaries toward `rustview-core`.
Core has no knowledge of the UI toolkit, and the relay has no dependency on screen
capture or input code. Platform-specific behavior remains at the desktop
application boundary.

The workspace uses Rust 2024 edition and Rust 1.92. `unsafe_code = "forbid"`
applies throughout the workspace.

## Components

### Desktop application

`rustview-desktop` supports two roles in the same binary:

- **Host:** Captures the screen, produces JPEG frames, displays the connection
  request to the local user, and applies approved input events.
- **Controller:** Connects using the nine-digit device ID and temporary access
  password, displays frames, and sends local mouse/keyboard events to the host when
  permission has been granted.

The UI uses `eframe/egui`, screen capture uses `xcap`, JPEG processing uses `image`,
and input generation uses `enigo` on supported platforms. These are MVP adapters;
platform capabilities are always checked at runtime.

### Core

`rustview-core` is responsible for:

- Persistent public device identity, temporary access passwords, and
  domain-separated relay-route/Noise-PSK derivation
- The internal/legacy `Invitation` (`RV1`) primitive, removed from the UI, and the
  lifecycle of its sensitive secret
- Relay control messages (`Register`, `Claim`, `Ping`, and responses)
- Size-bounded binary framing and `postcard` serialization
- Noise handshake and transport-message encryption
- Session messages, permissions, frame metadata, and protocol-version validation
- UI-independent state and error types

### Blind relay

`rustview-relay` is not a media server. It has two responsibilities:

1. Hold a host's `Register` request for a short time under the route derived from
   its device ID and temporary password.
2. Match a controller's `Claim` request for the same route, then copy bytes between
   the two TCP streams.

After pairing, the relay does not parse application messages and does not possess
the Noise key. The metadata visible to the relay and the limitations of this design
are documented in [SECURITY.md](SECURITY.md).

## Device ID, temporary password, and connection flow

The desktop UI uses two distinct user values:

- `DeviceId`: A nonzero, nine-digit public identifier generated once per
  installation. Leading zeros are allowed. Only this value is persisted in the
  `device-id` file in the user's configuration directory.
- `AccessPassword`: A temporary 16-character/80-bit password generated from the OS
  randomness source on every application launch using an unambiguous 32-symbol
  alphabet. It is never written to disk, is redacted from `Debug` output, and can
  be regenerated from the UI.

The desktop stores the public device ID and non-secret saved relay address in the
platform's user configuration directory. When `RUSTVIEW_CONFIG_DIR` is set,
`device-id` and `relay-address` are created in that directory. The override does not
persist the temporary password. Host and controller must use the same relay
address. Changing the relay address persists the setting and restarts host
registration.

The normalized identity and password are processed under two separate BLAKE2s
domains:

1. The `RustView password-protected route id` domain produces the 10-byte relay
   route.
2. The `RustView pairing secret` domain produces the 32-byte Noise PSK.

The route is not the first 10 bytes of the PSK; domain separation makes the outputs
independent. The desktop access path does not use a route derived from the device
ID alone. Consequently, knowing the public nine-digit ID is insufficient to
calculate or claim the relay route. Only the derived route is sent to the relay in
`Register`/`Claim`; the device ID, temporary password, and PSK are not sent in
plaintext.

Core's `Invitation` remains an internal primitive that carries these two derived
binary values into the existing secure-channel API. The
`RV1.<BASE32_ROUTE>.<BASE64URL_SECRET>` codec remains available for legacy, test,
and internal integration compatibility. The desktop UI no longer produces,
displays, or asks the user to paste `RV1` text. Because a serialized `RV1` still
contains the PSK, it must be treated as a secret capability.

The same temporary password can remain valid for multiple connection requests
during an application run. Each relay `Register` record is still consumed by one
`Claim`, and every new request requires separate, explicit local approval on the
host. Regenerating the password in the UI changes both the route and PSK and
restarts host registration.

```mermaid
sequenceDiagram
    participant H as Host
    participant R as Blind TCP relay
    participant C as Controller

    H->>H: Load persistent nine-digit ID; generate temporary 80-bit password
    H->>H: Derive route and 32-byte PSK from ID + password under separate domains
    H->>R: Register(route)
    H-->>C: Share the nine-digit ID and 16-character password through a secure channel
    C->>C: Derive the same route and PSK from ID + password
    C->>R: Claim(route)
    R-->>H: Registered / ClaimAccepted
    R-->>C: ClaimAccepted
    H<<->>C: Noise XXpsk0 handshake; relay forwards bytes only
    C->>H: Session and permission request
    H->>H: Local user approval
    H-->>C: Approved screen frames
    C-->>H: Approved input events only
```

The Noise suite is fixed and changes only with a protocol-version change:

```text
Noise_XXpsk0_25519_ChaChaPoly_BLAKE2s
```

TCP and the relay provide transport, while Noise provides confidentiality and
integrity between the endpoints and proves possession of the PSK derived from the
correct device ID and temporary password. The relay over raw TCP is not a
replacement for TLS; content security resides in the Noise layer.

Core supports supplying a static Noise key pair. The goal is to persist a device
key in the secure OS key store and pin a peer fingerprint. Until the first desktop
integration actually stores and pins that key, trust on first use is not
established; only possession of the PSK derived from the correct temporary password
is proven.

## Session state machine

The application flow is divided into the following security states:

```text
Idle
  -> Connecting
  -> NoiseHandshake
  -> AwaitingLocalConsent
  -> ViewOnly | Controlling
  -> Closing
  -> Idle
```

Core invariants:

- Application payloads are rejected until Noise is established.
- Frame capture does not begin, and input is not applied, before local approval.
- View permission is separate from control permission.
- The controller cannot elevate permissions remotely or dismiss the approval UI.
- Pressed keys and mouse buttons are released when the session closes.
- Protocol errors, timeouts, and authorization violations close the connection
  fail-closed.

## Media path and backpressure

The MVP path is:

```text
xcap BGRA/RGBA frame
  -> selected display
  -> scale to a 720p limit
  -> JPEG encode
  -> frame header and bounded payload
  -> Noise transport message
  -> TCP relay
  -> decode
  -> egui texture
```

On the MVP host, capture, encode, and send run serially in the same worker. This
prevents an unbounded queue of frames awaiting transmission, and a slow network
naturally lowers capture FPS. On the viewer, the decoded image occupies one slot;
if the UI falls behind, the newest frame replaces the old slot. JPEG byte size,
dimension metadata, and post-decode allocation are bounded; CPU-time and fuzzing
hardening remain for a later phase.

Packet loss under raw TCP creates head-of-line blocking for the entire stream. This
is an accepted MVP tradeoff. Media and input share the same encrypted TCP stream;
prioritizing small control messages separately is deferred to the QUIC/multi-stream
phase.

## Input and coordinates

Mouse coordinates are interpreted with each frame's selected-display identity,
physical pixel size, and scaling information. The controller maps coordinates in
the displayed image to the host screen. Incoming coordinates and key values are
validated before reaching the platform API.

The platform adapter attempts to enable the control backend only after local
approval. Wayland/XWayland is deliberately rejected. If macOS Accessibility
permission or another platform limitation blocks the backend, the grant is sent as
view-only.

## Relay scaling and operations

The initial relay is a single process with an in-memory pending-route table. The
MVP implements an absolute control deadline, pending-route TTL, disconnected-host
cleanup, tunnel idle/write timeouts, a two-hour absolute tunnel lifetime, and total
and per-IP concurrent-connection quotas. The following are also required before
production:

- An IP/route token bucket compatible with distributed deployment
- Per-session bandwidth and absolute session quotas
- Server-authenticated TLS 1.3 or QUIC transport
- Structured logs that redact sensitive values
- Health metrics that never log screen or encrypted payload data

If the relay restarts, pending route registrations may be lost. This is not user
data loss: the host registers again with the same ID/password during the same
application run. When the user regenerates the password, the route and PSK change
together.

## Future transport evolution

The TCP relay is the first working and easily debuggable path. In the long term,
the network layer will evolve in this order:

1. The current blind TCP relay and Noise PSK derived from the ID/password
2. QUIC streams/datagrams behind a transport trait
3. NAT traversal and relay fallback through `iroh` or an equivalent proven layer
4. Direct peer-to-peer connectivity when practical, with the relay used only for
   rendezvous/fallback
5. Separate priority/reliability policies for video and input

ICE, STUN, TURN, and NAT-traversal algorithms will not be implemented from scratch.
Moving to iroh/QUIC requires a separate threat-model review and protocol version.
The existing Noise credential binding will not be removed until it is bound to the
new transport's endpoint identity and handshake transcript.

## Test strategy

- Unit tests for device ID/temporary-password parsing, formatting, derivation, and
  invalid input
- Internal/legacy `Invitation`/`RV1` codec round-trip and redaction tests
- Framing size-limit and fragmented-TCP-read tests
- Noise test vectors and tests for PSKs derived from incorrect IDs/passwords,
  replay, and ciphertext mutation
- “No input before consent” property tests for the state machine
- Bounded-memory tests under slow consumers and disconnects
- Build/check/test CI on macOS, Windows, and Ubuntu
- Real-device permission, HiDPI, multi-display, and keyboard-layout tests
- Fuzz tests for the wire parser and JPEG metadata

Successful CI compilation does not prove platform behavior. Release support is
added to the platform table only after real-device smoke testing.
