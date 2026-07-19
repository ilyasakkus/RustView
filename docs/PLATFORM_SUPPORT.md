# Platform support

RustView targets a single cross-platform codebase, but screen capture and remote
input depend on each operating system's security model. “Compiles” and “supported”
are not the same. The table below shows the initial MVP target and known
limitations.

## Support summary

| Platform | UI | Screen capture | Remote input | Initial MVP status |
| --- | --- | --- | --- | --- |
| macOS 13+ Intel/Apple Silicon | Targeted | `xcap`; Screen Recording permission | `enigo`; Accessibility permission | Targeted; real-device validation required |
| Windows 10/11 x64 | Targeted | `xcap`; normal user desktop | `enigo`; normal user session | Targeted; real-device validation required |
| Linux X11 x86_64 | Targeted | `xcap` | `enigo` with `x11rb` | Targeted; distribution/desktop-environment testing required |
| Linux Wayland x86_64 | Targeted | `xcap` single-frame capture; depends on compositor/portal | Deliberately disabled | Experimental viewing; mandatory view-only fallback |

The MVP media target on every platform is a single selected display encoded as
720p JPEG at 5–10 FPS. Multi-display support, HDR, high refresh rates,
hardware-accelerated codecs, and system audio are not promised.

## macOS

### Target

- macOS 13 or later
- Release builds for Intel and Apple Silicon
- Screen sharing and approved control in a normal user session

### Permissions

1. **Screen Recording:** Required to capture screen or window content.
2. **Accessibility:** Separately required to generate mouse and keyboard events.

Users grant these permissions under System Settings → Privacy & Security. RustView
may need to be completely closed and reopened after permission is granted. macOS
may ask again when a binary's path or signature changes; this occurs more often for
development builds.

### Known limitations

- LoginWindow and some secure system surfaces are not shared or controlled.
- DRM or protected video may appear black.
- Keyboard layouts, Mission Control, and global shortcuts require further testing.
- The initial MVP uses `xcap`. ScreenCaptureKit is the long-term performance backend.
- The application must not be run as root.

## Windows

### Target

- 64-bit Windows 10 and Windows 11
- Screen sharing and approved control in a normal desktop session

### Permissions and limitations

- Normal desktop capture generally does not require an additional permission
  dialog.
- The **UAC secure desktop**, Ctrl+Alt+Del screen, and login screen cannot be
  captured or controlled in the MVP.
- RustView does not launch the entire application as Administrator and must not
  recommend doing so as a normal workaround.
- Input to Administrator windows may be restricted by process integrity levels.
- DRM/protected content may appear black.
- DPI scaling and multiple monitors with different scaling require separate
  validation.

The initial MVP uses `xcap` and `enigo`. A DXGI Desktop Duplication/Windows Graphics
Capture backend is planned for dirty rectangles, cursor shapes, and lower copying
costs.

## Linux/X11

### Target

- Ubuntu 24.04-based CI builds
- `xcap` capture and `enigo`/`x11rb` input on common X11 desktops

Required package names vary by distribution. Example for Ubuntu/Debian:

```bash
sudo apt-get install -y \
  libclang-dev pkg-config libdbus-1-dev libegl1-mesa-dev \
  libpipewire-0.3-dev libwayland-dev libx11-dev libxcb1-dev \
  libxkbcommon-dev libxrandr-dev
```

### Security note

X11 does not provide strong isolation between clients attached to the same display.
Another local X11 client may access screen/input data or generate synthetic input.
RustView's end-to-end network encryption does not address this local X11 risk.

### Known limitations

- Distribution, window manager, keyboard-layout, and XWayland combinations vary.
- Fractional scaling and negative monitor coordinates need additional testing.
- A headless X server is not an official MVP target.
- Running as root is not a supported workaround.

## Linux/Wayland

Wayland deliberately prevents applications from silently reading the screen or
generating global input. RustView does not bypass this model.

### Screen capture

The current MVP does not maintain a persistent PipeWire ScreenCast stream; it uses
`xcap`'s single-frame capture path for each frame. Depending on the compositor,
this path may use the GNOME screenshot API, XDG Screenshot portal, or wlroots
capture support. Registering with the relay for local access does not automatically
grant screen permission. The portal dialog may be repeated, capture may fail, and
the 5–10 FPS target for X11/macOS/Windows is not guaranteed on Wayland.

### Remote input

The current Linux build enables only the `x11rb` feature for `enigo`; Wayland/libei
features are not enabled. When the application detects a Wayland session through
`XDG_SESSION_TYPE`/`WAYLAND_DISPLAY`, it does not enable the native input backend,
even if an XWayland `DISPLAY` value exists. If capture works, a control request
falls back to view-only permission.

Production support requires jointly managing XDG RemoteDesktop and ScreenCast
portal sessions, mapping PipeWire stream coordinates to the input region, and
adding libei support. GNOME, KDE Plasma, and wlroots-based compositors must each be
validated separately on real devices.

## Shared display and input limitations

- The initial MVP shares one selected display.
- Correct HDR color conversion is not guaranteed; SDR JPEG is targeted.
- Depending on the platform, the cursor image/position may be embedded in the frame
  or handled separately.
- IMEs, dead keys, AltGr, media keys, and different keyboard layouts may not be
  fully supported.
- Coordinates must be clamped when controller and host use different DPI/scaling.
- Synthetically pressed keys and buttons must be released on disconnect.

## Criteria for marking a platform “supported”

Before a row moves from experimental to supported:

1. Installation and startup must be tested on a clean system.
2. Permission denial, later approval, and permission revocation flows must be tested.
3. At least 30 minutes of capture/view and control smoke testing must pass.
4. HiDPI and at least two keyboard layouts must be tested.
5. Input state must be cleared after disconnection.
6. Known limitations must be published in the release notes.

When contributing new platform results, include the operating-system version,
desktop environment/compositor, display protocol, architecture, and tested RustView
commit.
