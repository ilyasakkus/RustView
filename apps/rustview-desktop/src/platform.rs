//! Native screen capture and input-injection adapters.
//!
//! The rest of the desktop application depends on the small traits in this
//! module rather than exposing `xcap` or `enigo` types. This keeps protocol and
//! session code testable with fake backends and gives Wayland/native API work a
//! stable replacement boundary after the JPEG MVP.

use std::fmt;

use anyhow::{Context, Result, anyhow, bail};
use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use rustview_core::protocol::{ButtonState, InputEvent, MouseButton as ProtocolMouseButton};
use xcap::Monitor;

use crate::media::{self, EncodedFrame};

/// Stable display metadata used by the UI and input-coordinate mapper.
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorInfo {
    pub id: u32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale_factor: f32,
    pub is_primary: bool,
}

/// Screen-capture boundary used by the desktop session.
pub trait CaptureBackend {
    fn monitors(&self) -> Result<Vec<MonitorInfo>>;

    fn capture_jpeg(&self, monitor_id: u32, quality: u8) -> Result<EncodedFrame>;
}

/// Input-injection boundary used after session authorization succeeds.
pub trait InputBackend {
    fn inject(&mut self, event: &InputEvent) -> Result<()>;
}

/// `xcap` implementation of monitor enumeration and JPEG capture.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeCapture;

impl NativeCapture {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub fn monitors(&self) -> Result<Vec<MonitorInfo>> {
        <Self as CaptureBackend>::monitors(self)
    }

    pub fn capture_jpeg(&self, monitor_id: u32, quality: u8) -> Result<EncodedFrame> {
        <Self as CaptureBackend>::capture_jpeg(self, monitor_id, quality)
    }
}

impl CaptureBackend for NativeCapture {
    fn monitors(&self) -> Result<Vec<MonitorInfo>> {
        native_monitors()?
            .iter()
            .map(monitor_info)
            .collect::<Result<Vec<_>>>()
    }

    fn capture_jpeg(&self, monitor_id: u32, quality: u8) -> Result<EncodedFrame> {
        let monitor = native_monitors()?
            .into_iter()
            .find_map(|monitor| match monitor.id() {
                Ok(id) if id == monitor_id => Some(Ok(monitor)),
                Ok(_) => None,
                Err(error) => Some(Err(anyhow!(error))),
            })
            .transpose()
            .context("failed while matching the requested monitor")?
            .ok_or_else(|| anyhow!("monitor {monitor_id} is no longer available"))?;

        capture_monitor(&monitor, quality)
    }
}

fn native_monitors() -> Result<Vec<Monitor>> {
    let monitors = Monitor::all().context("failed to enumerate monitors")?;
    if monitors.is_empty() {
        bail!("no capturable monitors are available");
    }
    Ok(monitors)
}

fn monitor_info(monitor: &Monitor) -> Result<MonitorInfo> {
    let id = monitor.id().context("failed to read monitor id")?;
    let name = monitor
        .friendly_name()
        .or_else(|_| monitor.name())
        .unwrap_or_else(|_| format!("Display {id}"));
    let width = monitor.width().context("failed to read monitor width")?;
    let height = monitor.height().context("failed to read monitor height")?;
    if width == 0 || height == 0 {
        bail!("monitor {id} reported invalid dimensions {width}x{height}");
    }

    Ok(MonitorInfo {
        id,
        name,
        x: monitor.x().context("failed to read monitor x origin")?,
        y: monitor.y().context("failed to read monitor y origin")?,
        width,
        height,
        scale_factor: monitor
            .scale_factor()
            .context("failed to read monitor scale factor")?,
        is_primary: monitor
            .is_primary()
            .context("failed to read primary-monitor state")?,
    })
}

fn capture_monitor(monitor: &Monitor, quality: u8) -> Result<EncodedFrame> {
    let id = monitor.id().context("failed to read capture monitor id")?;
    let image = monitor
        .capture_image()
        .with_context(|| format!("failed to capture monitor {id}"))?;
    media::encode_jpeg(&image, quality)
        .with_context(|| format!("failed to prepare monitor {id} frame"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InputBounds {
    x: i32,
    y: i32,
    width: u32,
    height: u32,
}

impl InputBounds {
    fn new(x: i32, y: i32, width: u32, height: u32) -> Result<Self> {
        if width == 0 || height == 0 {
            bail!("input target dimensions must be non-zero, got {width}x{height}");
        }
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    fn map(self, x: u16, y: u16) -> Result<(i32, i32)> {
        Ok((
            normalized_axis(x, self.x, self.width)?,
            normalized_axis(y, self.y, self.height)?,
        ))
    }
}

/// `enigo` implementation for authorized protocol input events.
pub struct NativeInput {
    enigo: Enigo,
    target: InputBounds,
}

impl fmt::Debug for NativeInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NativeInput")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

impl NativeInput {
    /// Connect to the native input backend and initially target the main display.
    pub fn new() -> Result<Self> {
        ensure_remote_input_session_supported()?;
        let enigo = Enigo::new(&Settings::default())
            .context("failed to initialize native input injection")?;
        let (width, height) = enigo
            .main_display()
            .context("failed to query the main display for input mapping")?;
        let width = u32::try_from(width).context("main display width is negative")?;
        let height = u32::try_from(height).context("main display height is negative")?;

        Ok(Self {
            enigo,
            target: InputBounds::new(0, 0, width, height)?,
        })
    }

    /// Initialize input mapping for the same monitor that is being shared.
    pub fn for_monitor(monitor: &MonitorInfo) -> Result<Self> {
        let mut input = Self::new()?;
        input.set_target_monitor(monitor)?;
        Ok(input)
    }

    /// Update normalized pointer mapping after the shared monitor changes.
    pub fn set_target_monitor(&mut self, monitor: &MonitorInfo) -> Result<()> {
        self.target = InputBounds::new(monitor.x, monitor.y, monitor.width, monitor.height)?;
        Ok(())
    }

    pub fn inject(&mut self, event: &InputEvent) -> Result<()> {
        <Self as InputBackend>::inject(self, event)
    }
}

#[cfg(target_os = "linux")]
fn ensure_remote_input_session_supported() -> Result<()> {
    let session_type = std::env::var("XDG_SESSION_TYPE").ok();
    let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
    if is_wayland_session(session_type.as_deref(), wayland_display.as_deref()) {
        bail!(
            "remote input is disabled on Wayland until the XDG RemoteDesktop/libei backend is available"
        );
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn ensure_remote_input_session_supported() -> Result<()> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn is_wayland_session(session_type: Option<&str>, wayland_display: Option<&str>) -> bool {
    session_type.is_some_and(|value| value.eq_ignore_ascii_case("wayland"))
        || wayland_display.is_some_and(|value| !value.trim().is_empty())
}

impl InputBackend for NativeInput {
    fn inject(&mut self, event: &InputEvent) -> Result<()> {
        match *event {
            InputEvent::MouseMove { x, y } => {
                let (x, y) = self.target.map(x, y)?;
                self.enigo
                    .move_mouse(x, y, Coordinate::Abs)
                    .context("failed to inject pointer movement")
            }
            InputEvent::MouseButton { button, state } => self
                .enigo
                .button(mouse_button(button), direction(state))
                .context("failed to inject pointer button"),
            InputEvent::Scroll { delta_x, delta_y } => {
                if delta_x != 0 {
                    self.enigo
                        .scroll(i32::from(delta_x), Axis::Horizontal)
                        .context("failed to inject horizontal scroll")?;
                }
                if delta_y != 0 {
                    self.enigo
                        .scroll(i32::from(delta_y), Axis::Vertical)
                        .context("failed to inject vertical scroll")?;
                }
                Ok(())
            }
            InputEvent::Key { usage, state } => {
                let key = hid_usage_to_key(usage)
                    .ok_or_else(|| anyhow!("unsupported USB HID keyboard usage 0x{usage:04x}"))?;
                self.enigo
                    .key(key, direction(state))
                    .with_context(|| format!("failed to inject USB HID usage 0x{usage:04x}"))
            }
        }
    }
}

fn direction(state: ButtonState) -> Direction {
    match state {
        ButtonState::Pressed => Direction::Press,
        ButtonState::Released => Direction::Release,
    }
}

fn mouse_button(button: ProtocolMouseButton) -> Button {
    match button {
        ProtocolMouseButton::Left => Button::Left,
        ProtocolMouseButton::Right => Button::Right,
        ProtocolMouseButton::Middle => Button::Middle,
        ProtocolMouseButton::Back => Button::Back,
        ProtocolMouseButton::Forward => Button::Forward,
    }
}

fn normalized_axis(value: u16, origin: i32, extent: u32) -> Result<i32> {
    if extent == 0 {
        bail!("cannot map a normalized coordinate onto an empty axis");
    }

    let span = u64::from(extent - 1);
    let offset = (u64::from(value) * span + u64::from(u16::MAX) / 2) / u64::from(u16::MAX);
    let coordinate = i64::from(origin)
        .checked_add(i64::try_from(offset).context("pointer offset exceeds i64")?)
        .context("pointer coordinate overflow")?;
    i32::try_from(coordinate).context("pointer coordinate exceeds the native i32 range")
}

/// Convert USB HID Keyboard/Keypad page usages to semantic `enigo` keys.
///
/// Unknown usages are deliberately rejected instead of forwarding an untrusted
/// number as an OS-specific raw keycode.
fn hid_usage_to_key(usage: u16) -> Option<Key> {
    let key = match usage {
        0x04..=0x1d => {
            let codepoint = u32::from(b'a') + u32::from(usage - 0x04);
            Key::Unicode(char::from_u32(codepoint)?)
        }
        0x1e..=0x26 => {
            let codepoint = u32::from(b'1') + u32::from(usage - 0x1e);
            Key::Unicode(char::from_u32(codepoint)?)
        }
        0x27 => Key::Unicode('0'),
        0x28 => Key::Return,
        0x29 => Key::Escape,
        0x2a => Key::Backspace,
        0x2b => Key::Tab,
        0x2c => Key::Space,
        0x2d => Key::Unicode('-'),
        0x2e => Key::Unicode('='),
        0x2f => Key::Unicode('['),
        0x30 => Key::Unicode(']'),
        0x31 | 0x64 => Key::Unicode('\\'),
        0x32 => Key::Unicode('#'),
        0x33 => Key::Unicode(';'),
        0x34 => Key::Unicode('\''),
        0x35 => Key::Unicode('`'),
        0x36 => Key::Unicode(','),
        0x37 => Key::Unicode('.'),
        0x38 => Key::Unicode('/'),
        0x39 => Key::CapsLock,
        0x3a => Key::F1,
        0x3b => Key::F2,
        0x3c => Key::F3,
        0x3d => Key::F4,
        0x3e => Key::F5,
        0x3f => Key::F6,
        0x40 => Key::F7,
        0x41 => Key::F8,
        0x42 => Key::F9,
        0x43 => Key::F10,
        0x44 => Key::F11,
        0x45 => Key::F12,
        0x4a => Key::Home,
        0x4b => Key::PageUp,
        0x4c => Key::Delete,
        0x4d => Key::End,
        0x4e => Key::PageDown,
        0x4f => Key::RightArrow,
        0x50 => Key::LeftArrow,
        0x51 => Key::DownArrow,
        0x52 => Key::UpArrow,
        0x54 => Key::Divide,
        0x55 => Key::Multiply,
        0x56 => Key::Subtract,
        0x57 => Key::Add,
        0x58 => Key::Return,
        0x59 => Key::Numpad1,
        0x5a => Key::Numpad2,
        0x5b => Key::Numpad3,
        0x5c => Key::Numpad4,
        0x5d => Key::Numpad5,
        0x5e => Key::Numpad6,
        0x5f => Key::Numpad7,
        0x60 => Key::Numpad8,
        0x61 => Key::Numpad9,
        0x62 => Key::Numpad0,
        0x63 => Key::Decimal,
        0x67 => Key::Unicode('='),
        0x68 => Key::F13,
        0x69 => Key::F14,
        0x6a => Key::F15,
        0x6b => Key::F16,
        0x6c => Key::F17,
        0x6d => Key::F18,
        0x6e => Key::F19,
        0x6f => Key::F20,
        0x75 => Key::Help,
        0x7f => Key::VolumeMute,
        0x80 => Key::VolumeUp,
        0x81 => Key::VolumeDown,
        0xe0 => Key::LControl,
        0xe1 => Key::LShift,
        0xe2 | 0xe6 => Key::Alt,
        0xe3 | 0xe7 => Key::Meta,
        0xe4 => Key::RControl,
        0xe5 => Key::RShift,
        _ => return None,
    };
    Some(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_coordinates_include_both_endpoints() {
        assert_eq!(normalized_axis(0, 0, 1_920).unwrap(), 0);
        assert_eq!(normalized_axis(u16::MAX, 0, 1_920).unwrap(), 1_919);
    }

    #[test]
    fn normalized_coordinates_support_negative_monitor_origins() {
        assert_eq!(normalized_axis(0, -1_280, 1_280).unwrap(), -1_280);
        assert_eq!(normalized_axis(u16::MAX, -1_280, 1_280).unwrap(), -1);
    }

    #[test]
    fn standard_hid_usages_map_to_semantic_keys() {
        assert_eq!(hid_usage_to_key(0x04), Some(Key::Unicode('a')));
        assert_eq!(hid_usage_to_key(0x1d), Some(Key::Unicode('z')));
        assert_eq!(hid_usage_to_key(0x28), Some(Key::Return));
        assert_eq!(hid_usage_to_key(0xe0), Some(Key::LControl));
        assert_eq!(hid_usage_to_key(0xe5), Some(Key::RShift));
    }

    #[test]
    fn unknown_hid_usage_is_not_forwarded_as_a_raw_keycode() {
        assert_eq!(hid_usage_to_key(0xffff), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn wayland_sessions_fail_closed_even_when_xwayland_may_be_available() {
        assert!(is_wayland_session(Some("wayland"), None));
        assert!(is_wayland_session(Some("x11"), Some("wayland-0")));
        assert!(!is_wayland_session(Some("x11"), None));
        assert!(!is_wayland_session(None, Some("  ")));
    }
}
