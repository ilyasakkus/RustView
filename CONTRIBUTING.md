# Contributing to RustView

RustView is an open-source remote desktop project. Contributions involving code,
tests, documentation, design, accessibility, and platform validation are welcome.

## Before you begin

- Do not disclose vulnerabilities in a public issue or pull request; use the
  process in [SECURITY.md](SECURITY.md).
- Open a short design issue before making a major change to the protocol,
  cryptography, dependencies, or a platform backend.
- Read the current [architecture](docs/ARCHITECTURE.md),
  [security model](docs/SECURITY.md), and
  [platform limitations](docs/PLATFORM_SUPPORT.md).
- By contributing, you agree that your contribution will be distributed under the
  MIT License.

## Development environment

The required Rust version is defined in the workspace `rust-version` field and is
currently Rust 1.92. After installing the toolchain and components, run:

```bash
rustup component add rustfmt clippy
cargo build --workspace
cargo test --workspace
```

See the [platform documentation](docs/PLATFORM_SUPPORT.md) for native Linux
dependencies.

You can run the relay and desktop application in separate terminals:

```bash
cargo run -p rustview-relay -- --listen 127.0.0.1:21116
cargo run -p rustview-desktop
```

## Change workflow

1. Create a small branch focused on a single purpose.
2. Add tests alongside behavioral changes.
3. Update the relevant documentation for user-visible or protocol-visible changes.
4. Run the quality checks below locally.
5. Describe the scope, risks, and validated platforms in the pull request.

Required checks:

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same core checks on macOS, Windows, and Ubuntu. Successful compilation
on a platform does not prove runtime support; include real-device results for
platform-specific changes.

## Architecture rules

- UI and platform APIs must not be moved into `rustview-core`.
- The relay must not decode media or input payloads.
- Network input must be length-checked before allocation.
- Queue and connection counts must be bounded.
- Local host approval must not be bypassed; the default permission must remain
  view-only.
- New permissions and capabilities must be explicit and versioned in the protocol.
- `unsafe` is forbidden throughout the workspace. If OS FFI genuinely requires
  it, discuss the architecture first and make only a narrowly scoped policy change.
- Preserve the convention of using `thiserror` in library crates and `anyhow` for
  context at binary boundaries.
- Logs must not contain invitation secrets, key material, screen contents, or
  keystroke contents.

## Protocol and cryptography changes

Changes in the following areas are not considered “small refactors”; they require a
design and test vectors:

- The `RV1` invitation format or entropy
- The Noise pattern, cipher suite, or PSK placement
- Wire framing, maximum sizes, or message numbering
- The permission state machine
- Relay pairing or routing behavior
- Migration to QUIC, iroh, or another transport

Do not implement cryptographic primitives or PAKEs from scratch. A change proposal
must describe the threat-model impact, backward-compatibility decision, incorrect
credential/replay/mutation tests, and secret lifecycle.

## Testing expectations

Add the applicable tests for your change:

- Unit tests: parsing, validation, state transitions, and error paths
- Integration tests: fragmented TCP reads/writes, relay pairing, and disconnects
- Negative security tests: incorrect secrets, malformed ciphertext, replay, and
  limit violations
- Property/fuzz tests: wire parsers and allocation bounds
- UI/manual tests: permission denial, view-only fallback, and host disconnect
- Platform tests: OS version, architecture, display server/compositor, DPI, and
  keyboard layout

Remote-input tests may generate real mouse and keyboard actions. Run them in an
isolated environment and verify that input state is released when the test ends.

## Dependency policy

When adding a dependency:

- Review its maintenance status, audit history, use of `unsafe`, and transitive
  dependency count.
- Confirm that its license is compatible with distribution of this MIT-licensed
  project.
- Do not add a GPL codec or system FFmpeg dependency to a default feature.
- Explain the effect on platform builds and package size.
- When practical, disable default features and select only those that are needed.

## Documentation and language

English is the primary language for the application UI, source-facing text, and
repository documentation. Use clear, concise sentences and verifiable claims.
Translations are welcome as separate localized resources, but they must preserve
all security boundaries and must not strengthen claims beyond the English source.
Terms such as “E2EE,” “supported,” and “production-ready” must remain consistent
with the tests and threat model.

## Pull request checklist

- [ ] The change addresses one focused purpose.
- [ ] Tests were added or updated and pass locally.
- [ ] Formatting, checks, and Clippy pass.
- [ ] Documentation was updated if protocol or user behavior changed.
- [ ] New logs contain no sensitive data.
- [ ] New allocations, queues, and connections are bounded.
- [ ] Platform impact and manual test results are documented.
- [ ] Security and permission defaults were not weakened.
