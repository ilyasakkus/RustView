# Security policy

RustView takes security reports seriously. The project is in an early stage of
development and does not yet have an independently audited release that is
supported for production use.

## Supported versions

| Version | Security updates |
| --- | --- |
| Latest `main` / development version | Best effort |
| Old commits, forks, and modified builds | Not supported |

This table will be updated with semantic versions and support periods when the
first stable release is published.

## Reporting a vulnerability

Do not open a public GitHub issue, discussion, or pull request for a vulnerability.

The preferred channel is GitHub Private Vulnerability Reporting under
**Security → Advisories → Report a vulnerability** in the repository. If this
option is unavailable, contact the repository owner through a private channel
listed on their GitHub profile and do not place technical details in a public
forum.

When possible, include:

- The affected commit/release and operating system
- The affected component: desktop, core, device ID/password and Noise, framing, or
  relay
- Preconditions and step-by-step reproduction instructions
- Expected and actual behavior
- Impact: screen confidentiality, unauthorized input, key/secret disclosure, RCE,
  denial of service, and so on
- A minimal proof of concept, logs, and stack trace, if available
- A proposed fix or temporary mitigation

Do not send real access passwords, personal screenshots, credentials, or third-party
data. Use test data.

## Response process

On a best-effort basis, the maintainers will:

1. Privately acknowledge receipt of the report.
2. Assess the impact and reproducibility.
3. Coordinate a fix and disclosure timeline with the reporter.
4. Publish a CVE/GHSA and security advisory when appropriate.
5. Complete coordinated disclosure after the fix is released.

Because this is an early-stage volunteer project, no fixed response or remediation
SLA is guaranteed. If a critical issue is confirmed, users may be advised to stop
affected usage and shut down the relay.

## Areas of particular interest

- Access-password or internal invitation-secret disclosure through the relay,
  logs, or UI
- Noise handshake bypass, key/nonce reuse, or identity confusion
- The relay gaining access to plaintext screen or input data
- Capture or remote input before local approval
- Input being applied during a view-only session or permission escalation
- Panic, unbounded allocation, or code execution through framing, JPEG, or network
  input
- Unauthorized matching through route claiming or replay
- Synthetic input remaining pressed after disconnect
- Update, packaging, or supply-chain integrity

## Generally out of scope

The following are not vulnerabilities by themselves when they remain within the
documented limitations:

- The relay observing IP addresses, routes, traffic volume/timing, and session
  duration metadata
- A malicious relay dropping or delaying a connection
- Inability to control the Windows UAC secure desktop or login screen
- Wayland requiring portal approval or not supporting input
- X11's weak isolation between local clients
- A host or controller operating system that is already compromised at the
  Administrator/root level
- An authorized controller recording the screen with an external tool
- Automated scanner reports without a demonstrated theoretical and practical impact

If you are unsure whether an issue falls within a documented boundary, report it
privately anyway.

## Safe use

- Share your device ID and temporary access password only with people you trust.
- Protect the access password like a credential and send it through a secure channel.
- Review the requested permissions on the host screen.
- Do not grant control when it is unnecessary; use view-only mode.
- Monitor the active-session indicator and disconnect when finished.
- Do not run RustView as root or Administrator.
- Configure rate limits, timeouts, and log redaction before operating a public relay.

See [docs/SECURITY.md](docs/SECURITY.md) for the technical threat model and
cryptographic details.
