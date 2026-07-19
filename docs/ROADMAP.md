# RustView roadmap

RustView will be developed incrementally. Each milestone prioritizes a safe
approval flow and verifiable cross-platform behavior over an impressive feature
count. Dates are intentionally not fixed; a phase is not considered “supported”
until its acceptance criteria are complete.

## M0 — Project foundation

Status: **implementation complete; actual three-platform CI runs await repository
publication**

- Cargo workspace and the `rustview-core`, `rustview-desktop`, and
  `rustview-relay` packages
- Rust 2024/MSRV policy, formatting, Clippy, and three-platform CI
- Simple egui application shell
- Versioned wire messages, bounded framing, and shared error types
- Contribution, architecture, security, and platform documentation

Acceptance criteria:

- `cargo fmt --all -- --check`
- `cargo check --workspace --all-targets`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- Passing macOS, Windows, and Ubuntu CI jobs

## M1 — Remote desktop MVP through a secure relay

Status: **working prototype; real-device validation and security hardening continue**

- Persistent, public nine-digit device ID per installation
- A 16-character/80-bit temporary access password generated on every application
  launch and never written to disk, with copy and regenerate actions in the UI
- Derivation of a 10-byte relay route and 32-byte Noise PSK from the ID + password
  under separate domains; the route is not derived from the public ID alone
- Generation/parsing of the `RV1` invitation as an internal/legacy primitive; the
  user-facing UI uses separate ID and password fields
- Short-TTL, single-claim relay registration and automatic re-registration by the
  host
- Raw TCP `Register`/`Claim` rendezvous
- Blind byte relay that does not decrypt content
- `Noise_XXpsk0_25519_ChaChaPoly_BLAKE2s` handshake and transport
- Local connection approval on the host
- A separate password dialog after ID entry; mandatory explicit host approval even
  after password verification
- Single-display capture, scaling to 720p, and JPEG encoding/decoding
- A 5–10 FPS target and “latest frame wins” backpressure
- Display of the remote screen on the controller
- Separate view-only/control requests and explicit local host permission
- Basic mouse, button, scroll, and USB HID keyboard events
- Input session/grant epoch/sequence validation

Acceptance criteria:

- The relay must not obtain the access password or derived PSK in plaintext.
- Incorrect ID/password combinations and malformed or replayed handshakes must
  fail closed.
- Screen capture/transfer and input must not begin before approval.
- No input may be applied under a view-only grant.
- A slow controller must not cause unbounded memory growth.
- At least 30 minutes of smoke testing must pass in LAN and relay scenarios.

## M2 — Platform and control hardening

Status: **partially implemented**

- Permission/capability detection and safe view-only fallback on every platform
- Coordinate mapping for HiDPI and the selected monitor
- Release of synthetic input on disconnect, stop, and viewer focus loss
  (**implemented**)
- Prominent active-session indicator and one-action disconnect on the host
  (**implemented**)
- Mandatory view-only fallback in Wayland/XWayland sessions (**implemented**)
- UX for macOS Accessibility and Linux/Wayland capability fallback
- Tests for keyboard layouts, IMEs, modifiers, and special keys

Acceptance criteria:

- Property test: no input may be applied without local approval and `CONTROL`
  permission.
- Permissions must not be remotely elevated during a session.
- Every error path must release pressed keys/buttons.
- A real-device test matrix must be published for macOS, Windows, and Linux/X11.

## M3 — Distribution and security hardening

Status: **the first layer of relay resource limits is implemented; release
hardening is planned**

- Relay absolute control deadline, TTL, disconnected-host cleanup,
  idle/write/session timeouts, FD budget, and total and per-IP concurrent-connection
  quotas (**implemented**)
- Distributed token-bucket rate limiting with bandwidth/session quotas (**planned**)
- Server-authenticated TLS 1.3 or QUIC transport for the relay (**planned**)
- Fuzzing of device ID/password handling, the internal invitation, and framing;
  Noise negative/replay tests
- `cargo audit`, `cargo deny`, SBOM, and dependency policy
- Tracing/metrics with sensitive-data redaction
- macOS signing/notarization, Windows signing, and Linux packages
- Secure update design
- Independent threat-model and cryptographic-integration review

Acceptance criteria:

- No open high- or critical-severity security findings
- Packages install on clean macOS, Windows, and supported Linux environments
- Users can verify release-artifact integrity
- Resource consumption and abuse limits are documented and tested

## M4 — QUIC, NAT traversal, and direct connections

Status: **research/planned**

- Place network code behind a `PeerTransport` interface
- Prototype QUIC streams/datagrams
- Evaluate authenticated endpoints, NAT traversal, and encrypted relay fallback
  with iroh
- Design an application broker or address lookup
- Automatically fall back to the relay when a direct connection fails
- Separate priority/reliability policies for control and media

Acceptance criteria:

- Direct and relayed paths must use the same session state machine.
- Relay content confidentiality and credential-derived PSK binding must be
  preserved.
- A malicious coordinator must not bypass authentication through endpoint
  substitution.
- Input latency under packet loss must be measurably better than the TCP MVP.
- NAT traversal must not be written from scratch; the selected dependency and relay
  must be self-hostable.

## M5 — Media efficiency and platform backends

Status: **future**

- Native Windows DXGI/Desktop Duplication capture
- Native macOS ScreenCaptureKit capture
- Linux XDG ScreenCast/RemoteDesktop portal + PipeWire backend
- Dirty-region and cursor metadata
- Codec capability negotiation
- Hardware-accelerated H.264/AV1 or appropriately licensed alternatives
- Adaptive bitrate, resolution, and frame rate
- Multi-monitor selection and switching

Until this phase is complete, RustView will continue to state the 720p/5–10 FPS
JPEG MVP limit explicitly. Dependencies such as FFmpeg/x264 will not be added to
the default build without license and packaging review.

## M6 — Later product features

Status: **outside the MVP; not a commitment**

Possible features:

- Clipboard sharing with explicit, separate permission
- File transfer with sandboxing and user approval
- Audio streaming
- Address book and verified device fingerprints
- Accessibility and localization improvements
- Mobile viewer

Unattended access is explicitly a separate security project. The current persistent
nine-digit ID is a public locator, not an authentication key. Unattended access
will not be added until persistent device keys in a secure OS key store, persistent
access policy/PAKE, revocation, audit, and update models are complete.

## Continuous principles

- Secure defaults: view-only, local approval, and a temporary access password that
  lives only for the current application run
- Respect platform permission models instead of bypassing authorization
- Do not make the relay or coordinator a content trust root
- Bounded allocation and fail-closed protocol handling
- Real-device testing before marking a feature “supported”
- Fix vulnerabilities before adding new features
