//! Blocking network workers kept off the egui event loop.

use std::{
    net::{Shutdown, TcpStream, ToSocketAddrs},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
use rustview_core::{
    Invitation, Role, SecureChannel,
    framing::{read_message, write_message},
    protocol::{
        DisconnectReason, InputEvent, InputMessage, JpegFrameChunk, JpegFrameStart,
        MAX_JPEG_CHUNK_SIZE, MAX_JPEG_FRAME_SIZE, MouseButton, PROTOCOL_VERSION, PeerMessage,
        PermissionSet, RelayRequest, RelayResponse, SessionGrant, SessionRequest,
    },
    session::SessionState,
};
use tracing::{debug, warn};

use crate::{
    media,
    platform::{MonitorInfo, NativeCapture, NativeInput},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RELAY_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RELAY_REGISTRATION_WAIT: Duration = Duration::from_secs(10 * 60);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const SESSION_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const SESSION_GRANT_TIMEOUT: Duration = Duration::from_secs(75);
const SESSION_IO_TIMEOUT: Duration = Duration::from_secs(75);
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);
const CAPTURE_INTERVAL: Duration = Duration::from_millis(125);
const COMMAND_POLL_INTERVAL: Duration = Duration::from_millis(50);
const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);
const JPEG_QUALITY: u8 = 72;

#[derive(Clone, Debug)]
pub enum Command {
    Approve { remote_control: bool },
    Deny,
    Stop,
    Input(InputEvent),
}

#[derive(Clone, Debug)]
pub enum Event {
    Status(String),
    IncomingRequest {
        peer_name: String,
        requested_control: bool,
    },
    Active {
        remote_control: bool,
    },
    Stopped(String),
    Error(String),
}

#[derive(Clone, Debug)]
pub struct RemoteFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

pub struct Worker {
    pub commands: mpsc::Sender<Command>,
    pub events: mpsc::Receiver<Event>,
    latest_frame: Arc<Mutex<Option<RemoteFrame>>>,
}

#[derive(Clone, Debug)]
struct EventSink {
    sender: mpsc::SyncSender<Event>,
    terminal_emitted: Arc<AtomicBool>,
}

impl EventSink {
    fn new(sender: mpsc::SyncSender<Event>) -> Self {
        Self {
            sender,
            terminal_emitted: Arc::new(AtomicBool::new(false)),
        }
    }

    fn emit(&self, event: Event) {
        let is_terminal = matches!(&event, Event::Stopped(_) | Event::Error(_));
        if is_terminal
            && self
                .terminal_emitted
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return;
        }
        let _ = self.sender.send(event);
    }
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("Worker").finish_non_exhaustive()
    }
}

impl Worker {
    pub fn take_latest_frame(&self) -> Option<RemoteFrame> {
        self.latest_frame.lock().ok()?.take()
    }
}

pub fn start_host(relay_address: String, invitation: Invitation) -> Result<Worker> {
    validate_relay_address(&relay_address)?;
    spawn_worker("rustview-host", move |commands, events, _latest_frame| {
        host_worker(&relay_address, invitation, &commands, &events)
    })
}

pub fn start_viewer(
    relay_address: String,
    invitation: Invitation,
    request_control: bool,
) -> Result<Worker> {
    validate_relay_address(&relay_address)?;
    spawn_worker("rustview-viewer", move |commands, events, latest_frame| {
        viewer_worker(
            &relay_address,
            invitation,
            request_control,
            &commands,
            &events,
            latest_frame,
        )
    })
}

fn spawn_worker(
    name: &str,
    work: impl FnOnce(mpsc::Receiver<Command>, EventSink, Arc<Mutex<Option<RemoteFrame>>>) -> Result<()>
    + Send
    + 'static,
) -> Result<Worker> {
    let (command_sender, command_receiver) = mpsc::channel();
    // Status events are bounded; decoded pixels use a separate one-slot latest-frame buffer.
    let (event_sender, event_receiver) = mpsc::sync_channel(4);
    let event_sink = EventSink::new(event_sender);
    let latest_frame = Arc::new(Mutex::new(None));
    let worker_frame = Arc::clone(&latest_frame);
    let thread_name = name.to_owned();
    thread::Builder::new()
        .name(thread_name)
        .spawn(move || {
            let errors = event_sink.clone();
            if let Err(error) = work(command_receiver, event_sink, worker_frame) {
                errors.emit(Event::Error(format!("{error:#}")));
            }
        })
        .context("failed to start network worker thread")?;
    Ok(Worker {
        commands: command_sender,
        events: event_receiver,
        latest_frame,
    })
}

fn host_worker(
    relay_address: &str,
    invitation: Invitation,
    commands: &mpsc::Receiver<Command>,
    events: &EventSink,
) -> Result<()> {
    let result = host_worker_inner(relay_address, invitation, commands, events);
    if result.is_ok() {
        emit(events, Event::Stopped("Sharing session ended".to_owned()));
    }
    result
}

fn host_worker_inner(
    relay_address: &str,
    invitation: Invitation,
    commands: &mpsc::Receiver<Command>,
    events: &EventSink,
) -> Result<()> {
    emit(events, Event::Status("Connecting to relay".to_owned()));
    let mut stream = connect(relay_address)?;
    write_message(
        &mut stream,
        &RelayRequest::Register {
            version: PROTOCOL_VERSION,
            route_id: invitation.route_id(),
        },
    )?;
    let response = match wait_for_relay_response(
        &stream,
        commands,
        RELAY_RESPONSE_TIMEOUT,
        "relay registration response",
    )? {
        Some(response) => response,
        None => {
            emit(events, Event::Stopped("Connection cancelled".to_owned()));
            return Ok(());
        }
    };
    let ttl_secs = match response {
        RelayResponse::Registered { ttl_secs } if ttl_secs > 0 => ttl_secs,
        RelayResponse::Registered { .. } => {
            bail!("relay returned a zero-second registration lifetime")
        }
        other => return Err(relay_response_error(other)),
    };
    let ttl = Duration::from_secs(u64::from(ttl_secs)).min(MAX_RELAY_REGISTRATION_WAIT);
    emit(
        events,
        Event::Status(format!(
            "Waiting for a connection · relay registration valid for {} seconds",
            ttl.as_secs()
        )),
    );
    let response = match wait_for_relay_response(
        &stream,
        commands,
        ttl.saturating_add(RELAY_RESPONSE_TIMEOUT),
        "relay pairing response",
    )? {
        Some(response) => response,
        None => {
            emit(events, Event::Stopped("Connection cancelled".to_owned()));
            return Ok(());
        }
    };
    if response != RelayResponse::ClaimAccepted {
        return Err(relay_response_error(response));
    }

    emit(
        events,
        Event::Status("Establishing encrypted channel".to_owned()),
    );
    let channel = match establish_secure_channel(stream, Role::Host, &invitation, commands)? {
        Some(channel) => channel,
        None => {
            emit(events, Event::Stopped("Connection cancelled".to_owned()));
            return Ok(());
        }
    };
    channel.set_write_timeout(Some(SESSION_REQUEST_TIMEOUT))?;

    let request = match wait_for_peer_message(
        &channel,
        commands,
        SESSION_REQUEST_TIMEOUT,
        "session request",
    )? {
        Some(message) => match message {
            PeerMessage::SessionRequest(request) => request,
            PeerMessage::Disconnect { .. } => {
                emit(
                    events,
                    Event::Stopped("The remote peer closed the connection".to_owned()),
                );
                return Ok(());
            }
            _ => bail!("received an unexpected message before the session request"),
        },
        None => {
            emit(events, Event::Stopped("Connection cancelled".to_owned()));
            return Ok(());
        }
    };
    let mut state = SessionState::new();
    state.register_request(&request)?;
    let requested_control = request.requested_permissions.can_control();
    emit(
        events,
        Event::IncomingRequest {
            peer_name: request.viewer_name.clone(),
            requested_control,
        },
    );

    let approval_started = Instant::now();
    let requested_remote_control = loop {
        let remaining = APPROVAL_TIMEOUT.saturating_sub(approval_started.elapsed());
        if remaining.is_zero() {
            channel.send(&PeerMessage::Disconnect {
                reason: DisconnectReason::Timeout,
            })?;
            channel.shutdown()?;
            emit(events, Event::Stopped("Approval timed out".to_owned()));
            return Ok(());
        }
        match commands.recv_timeout(remaining.min(Duration::from_millis(250))) {
            Ok(Command::Approve { remote_control }) => {
                break remote_control && requested_control;
            }
            Ok(Command::Deny | Command::Stop) => {
                channel.send(&PeerMessage::Disconnect {
                    reason: DisconnectReason::Rejected,
                })?;
                channel.shutdown()?;
                emit(events, Event::Stopped("Connection rejected".to_owned()));
                return Ok(());
            }
            Ok(Command::Input(_)) | Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                channel.shutdown()?;
                return Ok(());
            }
        }
    };

    // Native capture/input allocation happens only after explicit local approval.
    let capture = NativeCapture::new();
    let monitors = capture.monitors().context("failed to enumerate displays")?;
    let monitor = choose_monitor(&monitors).context("no shareable display was found")?;
    let mut native_input = if requested_remote_control {
        match NativeInput::for_monitor(&monitor) {
            Ok(input) => Some(input),
            Err(error) => {
                warn!(%error, "native input backend is unavailable; using view-only mode");
                emit(
                    events,
                    Event::Status(
                        "Keyboard and mouse access is unavailable; using view-only mode".to_owned(),
                    ),
                );
                None
            }
        }
    } else {
        None
    };
    let remote_control = native_input.is_some();
    let granted_permissions = if remote_control {
        PermissionSet::VIEW_AND_CONTROL
    } else {
        PermissionSet::VIEW_ONLY
    };
    let grant = SessionGrant {
        request_id: request.request_id,
        session_id: random_id()?,
        grant_epoch: 1,
        granted_permissions,
    };
    state.activate(&grant)?;
    channel.send(&PeerMessage::SessionGrant(grant.clone()))?;
    configure_session_channel(&channel)?;
    emit(events, Event::Active { remote_control });

    let running = Arc::new(AtomicBool::new(true));
    let capture_thread = spawn_capture_loop(
        capture,
        monitor.id,
        channel.clone(),
        Arc::clone(&running),
        events.clone(),
    )?;
    let input_thread = spawn_input_loop(
        channel.clone(),
        Arc::clone(&running),
        state,
        native_input.take(),
        events.clone(),
    )?;

    while running.load(Ordering::Acquire) {
        match commands.recv_timeout(COMMAND_POLL_INTERVAL) {
            Ok(Command::Stop | Command::Deny) => {
                let _ = channel.send(&PeerMessage::Disconnect {
                    reason: DisconnectReason::Revoked,
                });
                break;
            }
            Ok(Command::Approve { .. } | Command::Input(_)) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    running.store(false, Ordering::Release);
    let _ = channel.shutdown();
    let _ = capture_thread.join();
    let _ = input_thread.join();
    Ok(())
}

fn viewer_worker(
    relay_address: &str,
    invitation: Invitation,
    request_control: bool,
    commands: &mpsc::Receiver<Command>,
    events: &EventSink,
    latest_frame: Arc<Mutex<Option<RemoteFrame>>>,
) -> Result<()> {
    emit(events, Event::Status("Connecting to relay".to_owned()));
    let mut stream = connect(relay_address)?;
    write_message(
        &mut stream,
        &RelayRequest::Claim {
            version: PROTOCOL_VERSION,
            route_id: invitation.route_id(),
        },
    )?;
    let response = match wait_for_relay_response(
        &stream,
        commands,
        RELAY_RESPONSE_TIMEOUT,
        "relay claim response",
    )? {
        Some(response) => response,
        None => return connection_cancelled(events),
    };
    if response != RelayResponse::ClaimAccepted {
        return Err(relay_response_error(response));
    }

    emit(
        events,
        Event::Status("Establishing encrypted channel".to_owned()),
    );
    let channel = match establish_secure_channel(stream, Role::Viewer, &invitation, commands)? {
        Some(channel) => channel,
        None => return connection_cancelled(events),
    };
    channel.set_write_timeout(Some(SESSION_REQUEST_TIMEOUT))?;
    let permissions = if request_control {
        PermissionSet::VIEW_AND_CONTROL
    } else {
        PermissionSet::VIEW_ONLY
    };
    let request = SessionRequest {
        request_id: random_id()?,
        viewer_name: local_viewer_name(),
        requested_permissions: permissions,
    };
    let mut state = SessionState::new();
    state.register_request(&request)?;
    channel.send(&PeerMessage::SessionRequest(request))?;
    emit(
        events,
        Event::Status("Waiting for approval on the remote device".to_owned()),
    );

    let grant = match wait_for_peer_message(
        &channel,
        commands,
        SESSION_GRANT_TIMEOUT,
        "session approval",
    )? {
        Some(message) => match message {
            PeerMessage::SessionGrant(grant) => grant,
            PeerMessage::Disconnect { reason } => {
                emit(events, Event::Stopped(disconnect_text(reason)));
                return Ok(());
            }
            _ => bail!("received an unexpected message before session approval"),
        },
        None => return connection_cancelled(events),
    };
    state.activate(&grant)?;
    configure_session_channel(&channel)?;
    let remote_control = grant.granted_permissions.can_control();
    emit(events, Event::Active { remote_control });

    let running = Arc::new(AtomicBool::new(true));
    let receiver = spawn_viewer_receive_loop(
        channel.clone(),
        Arc::clone(&running),
        state,
        events.clone(),
        latest_frame,
    )?;
    let mut sequence = 0_u64;
    let mut last_keepalive = Instant::now();
    while running.load(Ordering::Acquire) {
        match commands.recv_timeout(COMMAND_POLL_INTERVAL) {
            Ok(Command::Input(event))
                if grant
                    .granted_permissions
                    .contains(event.required_permission()) =>
            {
                let input = InputMessage {
                    session_id: grant.session_id,
                    grant_epoch: grant.grant_epoch,
                    sequence,
                    event,
                };
                channel.send(&PeerMessage::Input(input))?;
                sequence = sequence
                    .checked_add(1)
                    .context("input sequence number exhausted")?;
            }
            Ok(Command::Stop | Command::Deny) => {
                let _ = channel.send(&PeerMessage::Disconnect {
                    reason: DisconnectReason::Normal,
                });
                break;
            }
            Ok(Command::Approve { .. } | Command::Input(_)) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        if last_keepalive.elapsed() >= KEEPALIVE_INTERVAL {
            channel.send(&PeerMessage::Ping { nonce: sequence })?;
            last_keepalive = Instant::now();
        }
    }
    running.store(false, Ordering::Release);
    let _ = channel.shutdown();
    let _ = receiver.join();
    emit(events, Event::Stopped("Remote session ended".to_owned()));
    Ok(())
}

fn spawn_capture_loop(
    capture: NativeCapture,
    monitor_id: u32,
    channel: SecureChannel,
    running: Arc<AtomicBool>,
    events: EventSink,
) -> Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("rustview-capture".to_owned())
        .spawn(move || {
            let mut frame_id = 0_u64;
            while running.load(Ordering::Acquire) {
                let started = Instant::now();
                let result = (|| -> Result<()> {
                    let frame = capture.capture_jpeg(monitor_id, JPEG_QUALITY)?;
                    send_frame(&channel, frame_id, frame.width, frame.height, &frame.bytes)?;
                    frame_id = frame_id
                        .checked_add(1)
                        .context("frame sequence number exhausted")?;
                    Ok(())
                })();
                if let Err(error) = result {
                    if running.swap(false, Ordering::AcqRel) {
                        emit(
                            &events,
                            Event::Error(format!("Screen capture stopped: {error:#}")),
                        );
                    }
                    let _ = channel.shutdown();
                    return;
                }
                if let Some(delay) = CAPTURE_INTERVAL.checked_sub(started.elapsed()) {
                    thread::sleep(delay);
                }
            }
        })
        .context("failed to start screen capture thread")
}

fn spawn_input_loop(
    channel: SecureChannel,
    running: Arc<AtomicBool>,
    mut state: SessionState,
    mut input: Option<NativeInput>,
    events: EventSink,
) -> Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("rustview-input".to_owned())
        .spawn(move || {
            let mut pressed = PressedInputs::default();
            while running.load(Ordering::Acquire) {
                match channel.recv() {
                    Ok(PeerMessage::Input(message)) => {
                        let result = state
                            .authorize_input(&message)
                            .context("unauthorized input message")
                            .and_then(|()| {
                                input
                                    .as_mut()
                                    .context("remote-control backend is not active")?
                                    .inject(&message.event)?;
                                pressed.observe(message.event);
                                Ok(())
                            });
                        if let Err(error) = result {
                            emit(
                                &events,
                                Event::Error(format!("Remote input rejected: {error:#}")),
                            );
                            break;
                        }
                    }
                    Ok(PeerMessage::Ping { nonce }) => {
                        if channel.send(&PeerMessage::Pong { nonce }).is_err() {
                            break;
                        }
                    }
                    Ok(PeerMessage::Disconnect { reason }) => {
                        let message = disconnect_text(reason);
                        emit(&events, Event::Stopped(message));
                        break;
                    }
                    Ok(_) => {
                        emit(
                            &events,
                            Event::Error("Host received an unexpected protocol message".to_owned()),
                        );
                        break;
                    }
                    Err(error) if channel.is_closed() => {
                        debug!(%error, "host secure channel closed");
                        break;
                    }
                    Err(error) => {
                        emit(&events, Event::Error(format!("Connection lost: {error:#}")));
                        break;
                    }
                }
            }
            if let Some(input) = input.as_mut() {
                pressed.release_all(input);
            }
            running.store(false, Ordering::Release);
            let _ = channel.shutdown();
        })
        .context("failed to start remote input thread")
}

fn spawn_viewer_receive_loop(
    channel: SecureChannel,
    running: Arc<AtomicBool>,
    mut state: SessionState,
    events: EventSink,
    latest_frame: Arc<Mutex<Option<RemoteFrame>>>,
) -> Result<thread::JoinHandle<()>> {
    thread::Builder::new()
        .name("rustview-frame-receiver".to_owned())
        .spawn(move || {
            let mut assembler = FrameAssembler::default();
            while running.load(Ordering::Acquire) {
                let message = match channel.recv() {
                    Ok(message) => message,
                    Err(error) if channel.is_closed() => {
                        debug!(%error, "viewer secure channel closed");
                        break;
                    }
                    Err(error) => {
                        emit(&events, Event::Error(format!("Connection lost: {error:#}")));
                        break;
                    }
                };
                let result = match message {
                    PeerMessage::FrameStart(start) => state
                        .observe(&PeerMessage::FrameStart(start.clone()))
                        .map_err(anyhow::Error::from)
                        .and_then(|()| assembler.start(start)),
                    PeerMessage::FrameChunk(chunk) => state
                        .observe(&PeerMessage::FrameChunk(chunk.clone()))
                        .map_err(anyhow::Error::from)
                        .and_then(|()| assembler.push(chunk))
                        .and_then(|frame| {
                            if let Some(frame) = frame {
                                publish_frame(&latest_frame, frame)?;
                            }
                            Ok(())
                        }),
                    PeerMessage::Ping { nonce } => channel
                        .send(&PeerMessage::Pong { nonce })
                        .map_err(anyhow::Error::from),
                    PeerMessage::Pong { .. } => Ok(()),
                    PeerMessage::Disconnect { reason } => {
                        emit(&events, Event::Stopped(disconnect_text(reason)));
                        break;
                    }
                    _ => Err(anyhow!("viewer received an unexpected protocol message")),
                };
                if let Err(error) = result {
                    emit(
                        &events,
                        Event::Error(format!("Remote frame rejected: {error:#}")),
                    );
                    break;
                }
            }
            running.store(false, Ordering::Release);
            let _ = channel.shutdown();
        })
        .context("failed to start frame receiver thread")
}

fn send_frame(
    channel: &SecureChannel,
    frame_id: u64,
    width: u32,
    height: u32,
    bytes: &[u8],
) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_JPEG_FRAME_SIZE {
        bail!("JPEG frame size is outside the protocol limit");
    }
    let chunks = bytes.len().div_ceil(MAX_JPEG_CHUNK_SIZE);
    let chunk_count = u16::try_from(chunks).context("JPEG chunk count exceeds the u16 limit")?;
    channel.send(&PeerMessage::FrameStart(JpegFrameStart {
        frame_id,
        width,
        height,
        total_len: u32::try_from(bytes.len()).context("JPEG size exceeds the u32 limit")?,
        chunk_count,
    }))?;
    for (chunk_index, data) in bytes.chunks(MAX_JPEG_CHUNK_SIZE).enumerate() {
        channel.send(&PeerMessage::FrameChunk(JpegFrameChunk {
            frame_id,
            chunk_index: u16::try_from(chunk_index).context("chunk index exceeds the u16 limit")?,
            data: data.to_vec(),
        }))?;
    }
    Ok(())
}

#[derive(Debug)]
struct AssembledFrame {
    width: u32,
    height: u32,
    bytes: Vec<u8>,
}

#[derive(Debug)]
struct PendingFrame {
    start: JpegFrameStart,
    next_chunk: u16,
    bytes: Vec<u8>,
}

#[derive(Debug, Default)]
struct FrameAssembler {
    pending: Option<PendingFrame>,
}

impl FrameAssembler {
    fn start(&mut self, start: JpegFrameStart) -> Result<()> {
        start.validate()?;
        self.pending = Some(PendingFrame {
            bytes: Vec::with_capacity(start.total_len as usize),
            start,
            next_chunk: 0,
        });
        Ok(())
    }

    fn push(&mut self, chunk: JpegFrameChunk) -> Result<Option<AssembledFrame>> {
        chunk.validate()?;
        let pending = self.pending.as_mut().context("frame start is missing")?;
        if chunk.frame_id != pending.start.frame_id || chunk.chunk_index != pending.next_chunk {
            bail!("invalid frame chunk sequence");
        }
        if pending.bytes.len() + chunk.data.len() > pending.start.total_len as usize {
            bail!("frame exceeds its declared length");
        }
        pending.bytes.extend_from_slice(&chunk.data);
        pending.next_chunk = pending
            .next_chunk
            .checked_add(1)
            .context("frame chunk counter overflow")?;
        if pending.next_chunk != pending.start.chunk_count {
            return Ok(None);
        }
        if pending.bytes.len() != pending.start.total_len as usize {
            bail!("completed frame length does not match the declared value");
        }
        let completed = self.pending.take().expect("pending frame exists");
        Ok(Some(AssembledFrame {
            width: completed.start.width,
            height: completed.start.height,
            bytes: completed.bytes,
        }))
    }
}

fn publish_frame(
    latest_frame: &Arc<Mutex<Option<RemoteFrame>>>,
    frame: AssembledFrame,
) -> Result<()> {
    let decoded = media::decode_jpeg(&frame.bytes)?;
    if decoded.width() != frame.width || decoded.height() != frame.height {
        bail!("JPEG dimensions do not match the frame header");
    }
    let rgba = image::DynamicImage::ImageRgb8(decoded)
        .into_rgba8()
        .into_raw();
    let frame = RemoteFrame {
        width: frame.width,
        height: frame.height,
        rgba,
    };
    *latest_frame
        .lock()
        .map_err(|_| anyhow!("latest-frame mutex is poisoned"))? = Some(frame);
    Ok(())
}

#[derive(Debug, Default)]
struct PressedInputs {
    keys: Vec<u16>,
    buttons: Vec<MouseButton>,
}

impl PressedInputs {
    fn observe(&mut self, event: InputEvent) {
        match event {
            InputEvent::Key { usage, state } => update_pressed(&mut self.keys, usage, state),
            InputEvent::MouseButton { button, state } => {
                update_pressed(&mut self.buttons, button, state);
            }
            InputEvent::MouseMove { .. } | InputEvent::Scroll { .. } => {}
        }
    }

    fn release_all(&mut self, input: &mut NativeInput) {
        for usage in self.keys.drain(..) {
            let _ = input.inject(&InputEvent::Key {
                usage,
                state: rustview_core::protocol::ButtonState::Released,
            });
        }
        for button in self.buttons.drain(..) {
            let _ = input.inject(&InputEvent::MouseButton {
                button,
                state: rustview_core::protocol::ButtonState::Released,
            });
        }
    }
}

fn update_pressed<T: Copy + PartialEq>(
    values: &mut Vec<T>,
    value: T,
    state: rustview_core::protocol::ButtonState,
) {
    match state {
        rustview_core::protocol::ButtonState::Pressed if !values.contains(&value) => {
            values.push(value);
        }
        rustview_core::protocol::ButtonState::Released => values.retain(|item| *item != value),
        rustview_core::protocol::ButtonState::Pressed => {}
    }
}

fn connect(address: &str) -> Result<TcpStream> {
    let addresses = address
        .to_socket_addrs()
        .with_context(|| format!("failed to resolve relay address: {address}"))?;
    let mut last_error = None;
    for socket in addresses {
        match TcpStream::connect_timeout(&socket, CONNECT_TIMEOUT) {
            Ok(stream) => {
                stream.set_nodelay(true)?;
                stream.set_read_timeout(Some(CONNECT_TIMEOUT))?;
                stream.set_write_timeout(Some(CONNECT_TIMEOUT))?;
                return Ok(stream);
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.map_or_else(
        || anyhow!("relay address did not resolve to a valid network address"),
        |error| anyhow!(error).context(format!("failed to connect to relay: {address}")),
    ))
}

enum CancellableWait<T> {
    Completed(T),
    Cancelled,
    TimedOut,
    WorkerDisconnected,
}

fn poll_cancellable<T>(
    results: &mpsc::Receiver<T>,
    commands: &mpsc::Receiver<Command>,
    timeout: Duration,
) -> CancellableWait<T> {
    let started = Instant::now();
    loop {
        loop {
            match commands.try_recv() {
                Ok(Command::Stop | Command::Deny) | Err(mpsc::TryRecvError::Disconnected) => {
                    return CancellableWait::Cancelled;
                }
                Ok(Command::Approve { .. } | Command::Input(_)) => {}
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }

        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return CancellableWait::TimedOut;
        }
        match results.recv_timeout(remaining.min(COMMAND_POLL_INTERVAL)) {
            Ok(result) => return CancellableWait::Completed(result),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return CancellableWait::WorkerDisconnected;
            }
        }
    }
}

fn wait_for_relay_response(
    stream: &TcpStream,
    commands: &mpsc::Receiver<Command>,
    timeout: Duration,
    operation: &'static str,
) -> Result<Option<RelayResponse>> {
    let mut reader = stream.try_clone()?;
    reader.set_read_timeout(Some(timeout))?;
    let (result_sender, result_receiver) = mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name("rustview-relay-response".to_owned())
        .spawn(move || {
            let result = read_message::<_, RelayResponse>(&mut reader);
            let _ = result_sender.send(result);
        })
        .context("failed to start relay response worker")?;

    let outcome = poll_cancellable(&result_receiver, commands, timeout);
    if !matches!(&outcome, CancellableWait::Completed(_)) {
        let _ = stream.shutdown(Shutdown::Both);
    }
    let joined = worker.join();
    if joined.is_err() {
        bail!("{operation} worker panicked");
    }

    match outcome {
        CancellableWait::Completed(result) => Ok(Some(result?)),
        CancellableWait::Cancelled => Ok(None),
        CancellableWait::TimedOut => bail!("{operation} timed out"),
        CancellableWait::WorkerDisconnected => {
            bail!("worker stopped before the {operation} result was received")
        }
    }
}

fn establish_secure_channel(
    stream: TcpStream,
    role: Role,
    invitation: &Invitation,
    commands: &mpsc::Receiver<Command>,
) -> Result<Option<SecureChannel>> {
    stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
    let cancel_handle = stream.try_clone()?;
    let invitation = invitation.clone();
    let (result_sender, result_receiver) = mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name("rustview-noise-handshake".to_owned())
        .spawn(move || {
            let result = SecureChannel::establish(stream, role, &invitation);
            let _ = result_sender.send(result);
        })
        .context("failed to start encrypted-channel worker")?;

    let outcome = poll_cancellable(&result_receiver, commands, HANDSHAKE_TIMEOUT);
    if !matches!(&outcome, CancellableWait::Completed(_)) {
        let _ = cancel_handle.shutdown(Shutdown::Both);
    }
    let joined = worker.join();
    if joined.is_err() {
        bail!("encrypted-channel worker panicked");
    }

    match outcome {
        CancellableWait::Completed(result) => result
            .context("failed to establish end-to-end encrypted channel")
            .map(Some),
        CancellableWait::Cancelled => Ok(None),
        CancellableWait::TimedOut => bail!("encrypted-channel handshake timed out"),
        CancellableWait::WorkerDisconnected => {
            bail!("worker stopped before the encrypted-channel result was received")
        }
    }
}

fn wait_for_peer_message(
    channel: &SecureChannel,
    commands: &mpsc::Receiver<Command>,
    timeout: Duration,
    operation: &'static str,
) -> Result<Option<PeerMessage>> {
    channel.set_read_timeout(Some(timeout))?;
    let reader = channel.clone();
    let (result_sender, result_receiver) = mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name("rustview-session-setup".to_owned())
        .spawn(move || {
            let result = reader.recv();
            let _ = result_sender.send(result);
        })
        .context("failed to start session setup worker")?;

    let outcome = poll_cancellable(&result_receiver, commands, timeout);
    if !matches!(&outcome, CancellableWait::Completed(_)) {
        let _ = channel.shutdown();
    }
    let joined = worker.join();
    if joined.is_err() {
        bail!("{operation} worker panicked");
    }

    match outcome {
        CancellableWait::Completed(result) => result
            .with_context(|| format!("{operation} was not received within the allotted time"))
            .map(Some),
        CancellableWait::Cancelled => Ok(None),
        CancellableWait::TimedOut => bail!("{operation} timed out"),
        CancellableWait::WorkerDisconnected => {
            bail!("worker stopped before the {operation} result was received")
        }
    }
}

fn configure_session_channel(channel: &SecureChannel) -> Result<()> {
    channel.set_read_timeout(Some(SESSION_IO_TIMEOUT))?;
    channel.set_write_timeout(Some(SESSION_IO_TIMEOUT))?;
    Ok(())
}

fn connection_cancelled(events: &EventSink) -> Result<()> {
    emit(events, Event::Stopped("Connection cancelled".to_owned()));
    Ok(())
}

fn validate_relay_address(address: &str) -> Result<()> {
    if address.trim().is_empty() || address.trim() != address {
        bail!("relay address cannot be empty or contain leading or trailing whitespace");
    }
    Ok(())
}

fn choose_monitor(monitors: &[MonitorInfo]) -> Option<MonitorInfo> {
    monitors
        .iter()
        .find(|monitor| monitor.is_primary)
        .or_else(|| monitors.first())
        .cloned()
}

fn random_id<const N: usize>() -> Result<[u8; N]> {
    let mut output = [0_u8; N];
    getrandom::fill(&mut output).map_err(|_| anyhow!("failed to obtain OS randomness"))?;
    if output.iter().all(|byte| *byte == 0) {
        getrandom::fill(&mut output).map_err(|_| anyhow!("failed to obtain OS randomness"))?;
    }
    if output.iter().all(|byte| *byte == 0) {
        bail!("random identifier generation returned all zeroes");
    }
    Ok(output)
}

fn local_viewer_name() -> String {
    let candidate = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "RustView viewer".to_owned());
    let filtered: String = candidate
        .chars()
        .filter(|character| character.is_ascii_graphic() || *character == ' ')
        .take(rustview_core::protocol::MAX_VIEWER_NAME_LEN)
        .collect();
    if filtered.is_empty() {
        "RustView viewer".to_owned()
    } else {
        filtered
    }
}

fn relay_response_error(response: RelayResponse) -> anyhow::Error {
    match response {
        RelayResponse::Error { code } => anyhow!("relay rejected the request: {code:?}"),
        other => anyhow!("relay returned an unexpected response: {other:?}"),
    }
}

fn disconnect_text(reason: DisconnectReason) -> String {
    match reason {
        DisconnectReason::Normal => "The remote peer ended the session",
        DisconnectReason::Rejected => "Connection request rejected",
        DisconnectReason::Revoked => "Session permission revoked",
        DisconnectReason::Timeout => "Session timed out",
        DisconnectReason::ProtocolError => "Session closed because of a protocol error",
    }
    .to_owned()
}

fn emit(events: &EventSink, event: Event) {
    events.emit(event);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_preempts_a_pending_cancellable_wait() {
        let (result_sender, result_receiver) = mpsc::sync_channel::<()>(1);
        let (command_sender, command_receiver) = mpsc::channel();
        command_sender.send(Command::Stop).expect("queue stop");

        assert!(matches!(
            poll_cancellable(&result_receiver, &command_receiver, Duration::from_secs(1)),
            CancellableWait::Cancelled
        ));
        drop(result_sender);
    }

    #[test]
    fn frame_assembler_rejects_out_of_order_chunks() {
        let mut assembler = FrameAssembler::default();
        assembler
            .start(JpegFrameStart {
                frame_id: 7,
                width: 2,
                height: 2,
                total_len: 2,
                chunk_count: 2,
            })
            .unwrap();
        assert!(
            assembler
                .push(JpegFrameChunk {
                    frame_id: 7,
                    chunk_index: 1,
                    data: vec![1],
                })
                .is_err()
        );
    }

    #[test]
    fn frame_assembler_finishes_only_at_exact_length() {
        let mut assembler = FrameAssembler::default();
        assembler
            .start(JpegFrameStart {
                frame_id: 9,
                width: 2,
                height: 2,
                total_len: 2,
                chunk_count: 2,
            })
            .unwrap();
        assert!(
            assembler
                .push(JpegFrameChunk {
                    frame_id: 9,
                    chunk_index: 0,
                    data: vec![1],
                })
                .unwrap()
                .is_none()
        );
        let complete = assembler
            .push(JpegFrameChunk {
                frame_id: 9,
                chunk_index: 1,
                data: vec![2],
            })
            .unwrap()
            .unwrap();
        assert_eq!(complete.bytes, vec![1, 2]);
    }

    #[test]
    fn pressed_inputs_remove_released_values() {
        let mut keys = Vec::new();
        update_pressed(&mut keys, 4, rustview_core::protocol::ButtonState::Pressed);
        update_pressed(&mut keys, 4, rustview_core::protocol::ButtonState::Pressed);
        assert_eq!(keys, vec![4]);
        update_pressed(&mut keys, 4, rustview_core::protocol::ButtonState::Released);
        assert!(keys.is_empty());
    }

    #[test]
    fn terminal_event_is_emitted_only_once_across_sink_clones() {
        let (sender, receiver) = mpsc::sync_channel(2);
        let events = EventSink::new(sender);
        let worker_events = events.clone();

        emit(&worker_events, Event::Stopped("first shutdown".to_owned()));
        emit(&events, Event::Stopped("duplicate shutdown".to_owned()));

        assert!(matches!(
            receiver.try_recv(),
            Ok(Event::Stopped(message)) if message == "first shutdown"
        ));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }

    #[test]
    fn worker_error_is_not_overwritten_by_normal_completion() {
        let (sender, receiver) = mpsc::sync_channel(3);
        let events = EventSink::new(sender);
        let subthread_events = events.clone();

        emit(&events, Event::Status("active".to_owned()));
        emit(&subthread_events, Event::Error("capture failed".to_owned()));
        emit(&events, Event::Stopped("normal worker shutdown".to_owned()));

        assert!(matches!(
            receiver.try_recv(),
            Ok(Event::Status(message)) if message == "active"
        ));
        assert!(matches!(
            receiver.try_recv(),
            Ok(Event::Error(message)) if message == "capture failed"
        ));
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
}
