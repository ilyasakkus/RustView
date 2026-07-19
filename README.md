# RustView

RustView is an open-source, cross-platform remote desktop application written in
Rust. Its goal is to share a screen between macOS, Windows, and Linux and to
provide mouse and keyboard control only with the local user's explicit consent.

> [!WARNING]
> RustView is currently in an early stage of development. It has not undergone
> an independent security audit and is not a production-ready alternative to
> TeamViewer or AnyDesk. Do not share your access password with people you do
> not trust, and do not use RustView on sensitive systems.

## Download

Prebuilt RustView packages are published with each tagged release.

| Platform | Architecture | Download |
| --- | --- | --- |
| macOS 13+ | Intel and Apple silicon | [Download for macOS](https://github.com/ilyasakkus/RustView/releases/latest/download/RustView-macOS-universal.zip) |
| Windows | 64-bit x86 | [Download for Windows](https://github.com/ilyasakkus/RustView/releases/latest/download/RustView-Windows-x86_64.zip) |
| Linux | 64-bit x86 | [Download for Linux](https://github.com/ilyasakkus/RustView/releases/latest/download/RustView-Linux-x86_64.tar.gz) |

[View all releases](https://github.com/ilyasakkus/RustView/releases) ·
[Download SHA-256 checksums](https://github.com/ilyasakkus/RustView/releases/latest/download/SHA256SUMS.txt)

> [!IMPORTANT]
> RustView is currently distributed as an unsigned early-development build.
> macOS Gatekeeper and Windows SmartScreen may display a warning. Download
> packages only from this repository and verify the published SHA-256 checksums.

The direct links become available after the first tagged release is published.
RustView does not currently provide a hosted public relay service. Both computers
must use the same reachable relay address. Every release archive contains the
desktop application and the optional `rustview-relay` server.

### Run a downloaded package

#### macOS

1. Extract `RustView-macOS-universal.zip`.
2. Move `RustView.app` to the Applications folder.
3. Control-click `RustView.app`, select **Open**, and confirm the first launch if
   Gatekeeper blocks the unsigned build.
4. Grant Screen Recording permission when sharing the screen. Grant Accessibility
   permission only when keyboard and mouse control is required.

#### Windows

1. Extract the complete `RustView-Windows-x86_64.zip` archive.
2. Run `RustView.exe`.
3. Allow network access through Windows Firewall when prompted.

#### Linux

Extract the archive and start RustView:

```bash
tar -xzf RustView-Linux-x86_64.tar.gz
cd RustView-Linux-x86_64
./rustview
```

The Linux archive is dynamically linked and targets Ubuntu 22.04 or a compatible
distribution. The native libraries listed in [Requirements](#requirements) may
still be required on your system.

## Initial MVP scope

The first usable release is intentionally small:

- Share one display as approximately **720p, 5–10 FPS JPEG**
- Basic screen capture on macOS, Windows, and Linux/X11
- Basic mouse and keyboard control after local user approval
- End-to-end Noise encryption between the two clients
- A simple TCP rendezvous/relay service that forwards encrypted data without
  decrypting it
- A persistent, public nine-digit device ID; an 80-bit temporary access password
  regenerated on every application launch; and explicit local approval for every
  incoming request

The initial MVP does **not** include:

- Unattended access or a persistent password
- File transfer, clipboard synchronization, or audio streaming
- Control of the Windows UAC secure desktop or login screen
- Reliable remote input support on Wayland
- A hardware-accelerated video codec or AnyDesk-class latency/bandwidth
  performance
- Direct P2P/NAT traversal; both endpoints connect to the relay in the initial MVP

See the [roadmap](docs/ROADMAP.md) for detailed milestones and the
[platform support matrix](docs/PLATFORM_SUPPORT.md) for operating-system limits.

## How it works

On first launch, RustView generates a persistent, public nine-digit `DeviceId` for
the installation. On every application run, it also generates a random,
16-character, 80-bit `AccessPassword`. This password is never written to disk and
can be regenerated from the application.

The host's identity and temporary password are derived under separate domains into
a 10-byte relay route and a 32-byte Noise PSK. The route is not derived from the
public device ID alone. The relay receives only the route value; the device ID,
access password, and PSK are never sent in plaintext in the relay protocol.
Matched clients perform a `Noise_XXpsk0_25519_ChaChaPoly_BLAKE2s` handshake.
Screen and input data are transferred only after that handshake and after explicit
local approval on the host.

```mermaid
flowchart LR
    H["Host: capture and local approval"]
    R["Blind TCP relay: forwards encrypted bytes"]
    C["Controller: display and input"]
    H <-->|"End-to-end encrypted with Noise"| R
    R <-->|"End-to-end encrypted with Noise"| C
```

The relay cannot read the screen, input events, access password, or derived Noise
PSK. It can, however, observe IP addresses, route values, connection times, and
metadata such as traffic volume and timing. See the [security design](docs/SECURITY.md)
for details.

## Requirements

- Rust **1.92** or later
- A macOS, Windows, or Linux desktop environment
- Xcode Command Line Tools on macOS
- The MSVC Rust toolchain and the Visual Studio Build Tools “Desktop development
  with C++” components on Windows
- Native development packages for screen capture and windowing on Linux

Example dependencies for Ubuntu/Debian:

```bash
sudo apt-get update
sudo apt-get install -y \
  libclang-dev pkg-config libdbus-1-dev libegl1-mesa-dev \
  libpipewire-0.3-dev libwayland-dev libx11-dev libxcb1-dev \
  libxkbcommon-dev libxrandr-dev
```

Package names may vary by distribution. Wayland screen capture also requires a
working XDG Desktop Portal and PipeWire installation.

## Build and run from source

Install the platform requirements above and Rust 1.92 or later, then clone and
validate the entire workspace:

```bash
git clone https://github.com/ilyasakkus/RustView.git
cd RustView
cargo build --locked --workspace
cargo test --locked --workspace
```

Start the relay in one terminal:

```bash
cargo run --locked --release -p rustview-relay -- --listen 127.0.0.1:21116
```

The relay listens on `0.0.0.0:21116` by default. For local development, explicitly
binding the loopback address as shown above is safer. The address can also be set
with the `RUSTVIEW_RELAY_LISTEN` environment variable. The relay operator is
responsible for port and firewall configuration when testing over the internet.

> [!CAUTION]
> The MVP relay uses raw TCP. Noise protects screen and input content end to end,
> but the relay server itself is not yet authenticated with a TLS certificate.
> Before operating a public internet service, RustView requires TLS/QUIC,
> distributed rate limiting, bandwidth quotas, observability, and an independent
> security review.

Then launch the desktop application on the host and controller computers:

```bash
cargo run --locked --release -p rustview-desktop
```

The development flow is:

1. Select the same relay address in both applications.
2. Send the host's displayed nine-digit RustView ID and 16-character temporary
   password to the remote user through a secure channel.
3. The remote user enters the RustView ID, then enters the temporary password in
   the dialog that opens.
4. The remote user can request view-only access or keyboard/mouse control as well;
   control requests are disabled by default.
5. The host reviews the connecting party and requested permissions on the local
   screen and explicitly approves them. View and control permissions are evaluated
   separately.
6. The screen is shared while the session indicator remains active. The host can
   stop the session at any time.

The temporary password changes when the application restarts or when the
**Generate new** action is selected in the UI. Although a password can be used for
multiple connection requests during the same application run, every request still
requires new local approval on the host. RustView does not provide unattended
access.

Use the help output for relay CLI options. The relay address is saved from the
desktop UI and restored on the next launch. If set, the `RUSTVIEW_RELAY`
environment variable takes precedence over the saved value:

```bash
cargo run --locked -p rustview-relay -- --help
RUSTVIEW_RELAY=127.0.0.1:21116 cargo run --locked --release -p rustview-desktop
```

PowerShell equivalent:

```powershell
$env:RUSTVIEW_RELAY = "127.0.0.1:21116"
cargo run --locked --release -p rustview-desktop
```

RustView persists only the public device ID and the non-secret relay address
setting. The default locations for `device-id` and `relay-address` are
`~/Library/Application Support/RustView/` on macOS, `%APPDATA%\RustView\` on
Windows, and `$XDG_CONFIG_HOME/rustview/` on Linux (or `~/.config/rustview/` when
the variable is not set). For tests, portable installations, or custom packaging,
set `RUSTVIEW_CONFIG_DIR` to a directory; RustView creates both files there. The
temporary password is never written to that directory.

```bash
RUSTVIEW_CONFIG_DIR=/tmp/rustview-config cargo run --locked --release -p rustview-desktop
```

```powershell
$env:RUSTVIEW_CONFIG_DIR = "C:\Temp\rustview-config"
cargo run --locked --release -p rustview-desktop
```

## Platform permissions

- **macOS:** Screen Recording permission for capture and Accessibility permission
  for remote input are separate. The application may need to be restarted after
  permission is granted.
- **Windows:** The normal user desktop is targeted. The UAC secure desktop, login
  screen, and some protected content are inaccessible.
- **Linux/X11:** Screen capture and basic input are supported; the X11 security
  model does not adequately isolate applications from one another.
- **Linux/Wayland:** Screen capture depends on compositor/portal support and remains
  experimental. When the current MVP build detects a Wayland session, it does not
  enable input injection; control requests safely fall back to view-only mode.

RustView does not attempt to bypass permission screens and does not ask users to
run the entire application as Administrator or root.

## Repository layout

```text
apps/rustview-desktop/       egui/eframe desktop application
crates/rustview-core/        protocol, identity/password derivation, encryption, and shared types
services/rustview-relay/     blind TCP rendezvous/relay service
docs/                        architecture, security, and platform documentation
```

See [ARCHITECTURE.md](docs/ARCHITECTURE.md) for technical components and data flow.

## Contributing and security reports

Contributions are welcome. Read [CONTRIBUTING.md](CONTRIBUTING.md) before starting.
If you discover a vulnerability, do not open a public issue; follow the
[security reporting policy](SECURITY.md).

## License

RustView is available under the [MIT License](LICENSE-MIT).
