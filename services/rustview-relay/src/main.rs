use std::{
    collections::HashMap,
    io::{self, ErrorKind, Read, Write},
    net::{IpAddr, Shutdown, TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use rustview_core::{
    framing::{decode_message, write_message},
    protocol::{PROTOCOL_VERSION, RelayErrorCode, RelayRequest, RelayResponse, RouteId},
};
use tracing::{info, warn};

const DEFAULT_LISTEN: &str = "0.0.0.0:21116";
// A paired endpoint needs one additional duplicated socket for full-duplex copy.
// Keeping this at 96 caps the relay near 192 live socket descriptors, leaving
// headroom for the listener and transient control connections on common 256-FD
// development limits.
const MAX_CONNECTIONS: usize = 96;
const MAX_CONNECTIONS_PER_IP: usize = 16;
const CONNECTION_THREAD_STACK_SIZE: usize = 512 * 1024;
const MAX_RELAY_REQUEST_SIZE: usize = 256;
const INVITATION_TTL: Duration = Duration::from_secs(10 * 60);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(15);
const DEADLINE_POLL_INTERVAL: Duration = Duration::from_millis(5);
const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(100);
const HOST_LIVENESS_POLL_INTERVAL: Duration = Duration::from_secs(1);
const TUNNEL_READ_POLL_INTERVAL: Duration = Duration::from_secs(5);
const TUNNEL_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const TUNNEL_IDLE_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const TUNNEL_MAX_DURATION: Duration = Duration::from_secs(2 * 60 * 60);

static NEXT_REGISTRATION_ID: AtomicU64 = AtomicU64::new(1);

type ConnectionQuotas = Arc<Mutex<HashMap<IpAddr, usize>>>;
type WaitingHosts = Arc<Mutex<HashMap<RouteId, WaitingHost>>>;

struct ConnectionPermit {
    total: Arc<AtomicUsize>,
    per_ip: ConnectionQuotas,
    peer_ip: IpAddr,
}

impl ConnectionPermit {
    fn try_acquire(
        total: &Arc<AtomicUsize>,
        per_ip: &ConnectionQuotas,
        peer_ip: IpAddr,
    ) -> Option<Self> {
        if total.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            total.fetch_sub(1, Ordering::AcqRel);
            return None;
        }

        let mut quotas = per_ip.lock().expect("connection-quota mutex poisoned");
        let connections = quotas.entry(peer_ip).or_default();
        if *connections >= MAX_CONNECTIONS_PER_IP {
            total.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        *connections += 1;
        drop(quotas);

        Some(Self {
            total: Arc::clone(total),
            per_ip: Arc::clone(per_ip),
            peer_ip,
        })
    }
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        let previous = self.total.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0, "connection counter underflow");

        let mut quotas = self.per_ip.lock().expect("connection-quota mutex poisoned");
        let remove_entry = if let Some(connections) = quotas.get_mut(&self.peer_ip) {
            debug_assert!(*connections > 0, "per-IP connection counter underflow");
            *connections = connections.saturating_sub(1);
            *connections == 0
        } else {
            false
        };
        if remove_entry {
            quotas.remove(&self.peer_ip);
        }
    }
}

struct PendingViewer {
    stream: TcpStream,
    permit: Option<ConnectionPermit>,
}

struct WaitingHost {
    registration_id: u64,
    sender: mpsc::SyncSender<PendingViewer>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "rustview_relay=info".into()),
        )
        .init();

    let listen = listen_address()?;
    let listener = TcpListener::bind(&listen)
        .with_context(|| format!("relay {listen} adresini dinleyemedi"))?;
    info!(address = %listen, "RustView blind relay hazır");

    let waiting = WaitingHosts::default();
    let total_connections = Arc::new(AtomicUsize::new(0));
    let connection_quotas = ConnectionQuotas::default();
    for incoming in listener.incoming() {
        let mut stream = match incoming {
            Ok(stream) => stream,
            Err(error) => {
                warn!(%error, "bağlantı kabul edilemedi");
                thread::sleep(ACCEPT_ERROR_BACKOFF);
                continue;
            }
        };

        let peer_ip = match stream.peer_addr() {
            Ok(peer) => peer.ip(),
            Err(error) => {
                let _ = stream.shutdown(Shutdown::Both);
                warn!(%error, "bağlantının kaynak adresi belirlenemedi");
                continue;
            }
        };
        let Some(permit) =
            ConnectionPermit::try_acquire(&total_connections, &connection_quotas, peer_ip)
        else {
            reject_rate_limited(&mut stream);
            warn!(%peer_ip, "eşzamanlı bağlantı kotası aşıldı");
            continue;
        };

        let waiting = Arc::clone(&waiting);
        let spawn_result = thread::Builder::new()
            .name("rustview-relay-connection".to_owned())
            .stack_size(CONNECTION_THREAD_STACK_SIZE)
            .spawn(move || {
                if let Err(error) = handle_connection(stream, &waiting, Some(permit)) {
                    warn!(%error, "relay bağlantısı kapandı");
                }
            });
        if let Err(error) = spawn_result {
            warn!(%error, %peer_ip, "relay bağlantı thread'i başlatılamadı");
        }
    }
    Ok(())
}

fn reject_rate_limited(stream: &mut TcpStream) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let _ = write_message(
        stream,
        &RelayResponse::Error {
            code: RelayErrorCode::RateLimited,
        },
    );
    let _ = stream.shutdown(Shutdown::Both);
}

fn listen_address() -> Result<String> {
    let mut arguments = std::env::args().skip(1);
    let mut listen =
        std::env::var("RUSTVIEW_RELAY_LISTEN").unwrap_or_else(|_| DEFAULT_LISTEN.to_owned());
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--listen" => {
                listen = arguments
                    .next()
                    .context("--listen için HOST:PORT değeri gerekli")?;
            }
            "--help" | "-h" => {
                println!("RustView blind relay\n\nUSAGE:\n  rustview-relay [--listen HOST:PORT]");
                std::process::exit(0);
            }
            unknown => bail!("bilinmeyen argüman: {unknown}"),
        }
    }
    Ok(listen)
}

fn handle_connection(
    mut stream: TcpStream,
    waiting: &WaitingHosts,
    permit: Option<ConnectionPermit>,
) -> Result<()> {
    stream.set_nodelay(true)?;
    stream.set_write_timeout(Some(CONTROL_TIMEOUT))?;
    let request = read_relay_request(&mut stream, CONTROL_TIMEOUT)?;
    if request.validate().is_err() {
        let code = match request {
            RelayRequest::Register { version, .. } | RelayRequest::Claim { version, .. }
                if version != PROTOCOL_VERSION =>
            {
                RelayErrorCode::VersionMismatch
            }
            _ => RelayErrorCode::InvalidRequest,
        };
        write_message(&mut stream, &RelayResponse::Error { code })?;
        return Ok(());
    }
    match request {
        RelayRequest::Register { route_id, .. } => register_host(stream, waiting, route_id, permit),
        RelayRequest::Claim { route_id, .. } => claim_host(stream, waiting, route_id, permit),
        RelayRequest::Ping { nonce } => {
            write_message(&mut stream, &RelayResponse::Pong { nonce })?;
            Ok(())
        }
    }
}

fn read_relay_request(stream: &mut TcpStream, timeout: Duration) -> Result<RelayRequest> {
    stream.set_read_timeout(None)?;
    stream.set_nonblocking(true)?;
    let deadline = Instant::now()
        .checked_add(timeout)
        .context("relay kontrol zaman aşımı hesaplanamadı")?;

    let result = (|| {
        let mut prefix = [0_u8; 2];
        read_exact_until(stream, &mut prefix, deadline)?;
        let length = usize::from(u16::from_be_bytes(prefix));
        if length == 0 {
            bail!("boş relay kontrol çerçevesi");
        }
        if length > MAX_RELAY_REQUEST_SIZE {
            bail!("relay kontrol çerçevesi çok büyük: {length} > {MAX_RELAY_REQUEST_SIZE}");
        }

        let mut encoded = vec![0_u8; length];
        read_exact_until(stream, &mut encoded, deadline)?;
        decode_message(&encoded).context("relay kontrol isteği çözümlenemedi")
    })();

    let blocking_result = stream.set_nonblocking(false);
    match (result, blocking_result) {
        (Ok(request), Ok(())) => Ok(request),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error.into()),
    }
}

fn read_exact_until(
    stream: &mut TcpStream,
    output: &mut [u8],
    deadline: Instant,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < output.len() {
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                ErrorKind::TimedOut,
                "relay kontrol isteği zaman aşımına uğradı",
            ));
        }

        match stream.read(&mut output[offset..]) {
            Ok(0) => return Err(io::Error::from(ErrorKind::UnexpectedEof)),
            Ok(read) => offset += read,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(DEADLINE_POLL_INTERVAL.min(deadline.saturating_duration_since(now)));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn register_host(
    mut host: TcpStream,
    waiting: &WaitingHosts,
    route_id: RouteId,
    host_permit: Option<ConnectionPermit>,
) -> Result<()> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let registration_id = NEXT_REGISTRATION_ID.fetch_add(1, Ordering::Relaxed);
    {
        let mut routes = waiting.lock().expect("waiting-host mutex poisoned");
        if routes.contains_key(&route_id) {
            write_message(
                &mut host,
                &RelayResponse::Error {
                    code: RelayErrorCode::Busy,
                },
            )?;
            return Ok(());
        }
        routes.insert(
            route_id,
            WaitingHost {
                registration_id,
                sender,
            },
        );
    }

    if let Err(error) = write_message(
        &mut host,
        &RelayResponse::Registered {
            ttl_secs: INVITATION_TTL.as_secs() as u32,
        },
    ) {
        remove_registration(waiting, &route_id, registration_id);
        return Err(error.into());
    }
    info!(route = %route_label(&route_id), "host bekliyor");
    if let Err(error) = host.set_nonblocking(true) {
        remove_registration(waiting, &route_id, registration_id);
        return Err(error.into());
    }

    let expires_at = Instant::now() + INVITATION_TTL;
    let result = loop {
        let remaining = expires_at.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            remove_registration(waiting, &route_id, registration_id);
            let _ = host.set_nonblocking(false);
            let _ = write_message(
                &mut host,
                &RelayResponse::Error {
                    code: RelayErrorCode::Unavailable,
                },
            );
            info!(route = %route_label(&route_id), "davet zaman aşımına uğradı");
            break Ok(());
        }

        match receiver.recv_timeout(remaining.min(HOST_LIVENESS_POLL_INTERVAL)) {
            Ok(PendingViewer {
                stream: mut viewer,
                permit: viewer_permit,
            }) => {
                host.set_nonblocking(false)?;
                if let Err(error) = write_message(&mut host, &RelayResponse::ClaimAccepted) {
                    let _ = write_message(
                        &mut viewer,
                        &RelayResponse::Error {
                            code: RelayErrorCode::Unavailable,
                        },
                    );
                    break Err(error.into());
                }
                write_message(&mut viewer, &RelayResponse::ClaimAccepted)?;
                info!(route = %route_label(&route_id), "uçlar eşleştirildi");
                let proxy_result = proxy(host, viewer).context("tünel hatası");
                drop(viewer_permit);
                break proxy_result;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => match waiting_host_is_unusable(&host) {
                Ok(true) => {
                    remove_registration(waiting, &route_id, registration_id);
                    info!(route = %route_label(&route_id), "host ayrıldı; davet kaldırıldı");
                    break Ok(());
                }
                Ok(false) => {}
                Err(error) => {
                    remove_registration(waiting, &route_id, registration_id);
                    break Err(error.into());
                }
            },
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                remove_registration(waiting, &route_id, registration_id);
                break Ok(());
            }
        }
    };
    drop(host_permit);
    result
}

fn waiting_host_is_unusable(stream: &TcpStream) -> io::Result<bool> {
    let mut byte = [0_u8; 1];
    match stream.peek(&mut byte) {
        Ok(0) => Ok(true),
        // The host must wait for ClaimAccepted before sending any tunnel bytes.
        // Pending data therefore means a protocol violation (and also catches
        // data followed by FIN, which a plain EOF-only probe would retain).
        Ok(_) => Ok(true),
        Err(error) if error.kind() == ErrorKind::WouldBlock => Ok(false),
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::ConnectionAborted
                    | ErrorKind::ConnectionReset
                    | ErrorKind::NotConnected
                    | ErrorKind::BrokenPipe
            ) =>
        {
            Ok(true)
        }
        Err(error) => Err(error),
    }
}

fn remove_registration(waiting: &WaitingHosts, route_id: &RouteId, registration_id: u64) {
    let mut routes = waiting.lock().expect("waiting-host mutex poisoned");
    let matches_registration = routes
        .get(route_id)
        .is_some_and(|host| host.registration_id == registration_id);
    if matches_registration {
        routes.remove(route_id);
    }
}

fn claim_host(
    mut viewer: TcpStream,
    waiting: &WaitingHosts,
    route_id: RouteId,
    permit: Option<ConnectionPermit>,
) -> Result<()> {
    let waiting_host = waiting
        .lock()
        .expect("waiting-host mutex poisoned")
        .remove(&route_id);
    if let Some(waiting_host) = waiting_host {
        let pending = PendingViewer {
            stream: viewer,
            permit,
        };
        if let Err(send_error) = waiting_host.sender.send(pending) {
            let PendingViewer {
                stream: mut viewer,
                permit: _permit,
            } = send_error.0;
            write_message(
                &mut viewer,
                &RelayResponse::Error {
                    code: RelayErrorCode::Unavailable,
                },
            )?;
        }
    } else {
        write_message(
            &mut viewer,
            &RelayResponse::Error {
                code: RelayErrorCode::Unavailable,
            },
        )?;
    }
    Ok(())
}

fn proxy(host: TcpStream, viewer: TcpStream) -> io::Result<()> {
    host.set_read_timeout(Some(TUNNEL_READ_POLL_INTERVAL))?;
    host.set_write_timeout(Some(TUNNEL_WRITE_TIMEOUT))?;
    viewer.set_read_timeout(Some(TUNNEL_READ_POLL_INTERVAL))?;
    viewer.set_write_timeout(Some(TUNNEL_WRITE_TIMEOUT))?;

    let mut host_reader = host.try_clone()?;
    let mut viewer_writer = viewer.try_clone()?;
    let last_activity = Arc::new(Mutex::new(Instant::now()));
    let started_at = Instant::now();
    let first_activity = Arc::clone(&last_activity);
    let first = thread::Builder::new()
        .name("rustview-relay-copy".to_owned())
        .stack_size(CONNECTION_THREAD_STACK_SIZE)
        .spawn(move || {
            let result = copy_with_idle_timeout(
                &mut host_reader,
                &mut viewer_writer,
                &first_activity,
                started_at,
            );
            shutdown_tunnel(&host_reader, &viewer_writer);
            result
        })?;

    let mut viewer_reader = viewer;
    let mut host_writer = host;
    let second_result = copy_with_idle_timeout(
        &mut viewer_reader,
        &mut host_writer,
        &last_activity,
        started_at,
    );
    shutdown_tunnel(&viewer_reader, &host_writer);
    let first_result = first
        .join()
        .map_err(|_| io::Error::other("relay kopyalama thread'i panikledi"))?;

    match (first_result, second_result) {
        (Err(error), _) | (_, Err(error))
            if !matches!(
                error.kind(),
                ErrorKind::ConnectionAborted
                    | ErrorKind::ConnectionReset
                    | ErrorKind::BrokenPipe
                    | ErrorKind::UnexpectedEof
                    | ErrorKind::NotConnected
                    | ErrorKind::TimedOut
            ) =>
        {
            Err(error)
        }
        _ => Ok(()),
    }
}

fn copy_with_idle_timeout<R, W>(
    reader: &mut R,
    writer: &mut W,
    last_activity: &Arc<Mutex<Instant>>,
    started_at: Instant,
) -> io::Result<u64>
where
    R: Read,
    W: Write,
{
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 32 * 1024];
    loop {
        if started_at.elapsed() >= TUNNEL_MAX_DURATION {
            return Err(io::Error::new(
                ErrorKind::TimedOut,
                "relay tüneli mutlak oturum süresini aştı",
            ));
        }
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(copied),
            Ok(read) => {
                writer.write_all(&buffer[..read])?;
                copied = copied.saturating_add(read as u64);
                *last_activity.lock().expect("activity mutex poisoned") = Instant::now();
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                let idle_for = last_activity
                    .lock()
                    .expect("activity mutex poisoned")
                    .elapsed();
                if idle_for >= TUNNEL_IDLE_TIMEOUT {
                    return Err(io::Error::new(
                        ErrorKind::TimedOut,
                        "relay tüneli boşta kalma süresini aştı",
                    ));
                }
            }
            Err(error) => return Err(error),
        }
    }
}

fn shutdown_tunnel(host: &TcpStream, viewer: &TcpStream) {
    let _ = host.shutdown(Shutdown::Both);
    let _ = viewer.shutdown(Shutdown::Both);
}

fn route_label(route_id: &RouteId) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}…",
        route_id[0], route_id[1], route_id[2], route_id[3]
    )
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, TcpListener, TcpStream},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    use rustview_core::{
        framing::{read_message, write_message},
        protocol::{PROTOCOL_VERSION, RelayRequest, RelayResponse},
    };

    use super::{
        ConnectionPermit, ConnectionQuotas, MAX_CONNECTIONS, MAX_CONNECTIONS_PER_IP, WaitingHosts,
        handle_connection, proxy, read_relay_request, route_label,
    };

    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("local address");
        let client = TcpStream::connect(address).expect("connect");
        let (server, _) = listener.accept().expect("accept");
        (client, server)
    }

    #[test]
    fn route_labels_are_stable_and_do_not_contain_secrets() {
        assert_eq!(route_label(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 255]), "00010203…");
    }

    #[test]
    fn relay_pairs_two_clients_and_forwards_opaque_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("local address");
        let waiting = WaitingHosts::default();
        let total = Arc::new(AtomicUsize::new(0));
        let quotas = ConnectionQuotas::default();
        let server_total = Arc::clone(&total);
        let server_quotas = Arc::clone(&quotas);
        let server = thread::spawn(move || {
            let mut handlers = Vec::new();
            for _ in 0..2 {
                let (stream, peer) = listener.accept().expect("accept");
                let waiting = Arc::clone(&waiting);
                let permit =
                    ConnectionPermit::try_acquire(&server_total, &server_quotas, peer.ip())
                        .expect("connection permit");
                handlers.push(thread::spawn(move || {
                    handle_connection(stream, &waiting, Some(permit)).expect("handle connection");
                }));
            }
            for handler in handlers {
                handler.join().expect("relay handler");
            }
        });

        let timeout = Some(Duration::from_secs(2));
        let route_id = [7_u8; 10];
        let mut host = TcpStream::connect(address).expect("connect host");
        host.set_read_timeout(timeout).unwrap();
        host.set_write_timeout(timeout).unwrap();
        write_message(
            &mut host,
            &RelayRequest::Register {
                version: PROTOCOL_VERSION,
                route_id,
            },
        )
        .expect("register host");
        assert!(matches!(
            read_message::<_, RelayResponse>(&mut host).expect("registered"),
            RelayResponse::Registered { .. }
        ));

        let mut viewer = TcpStream::connect(address).expect("connect viewer");
        viewer.set_read_timeout(timeout).unwrap();
        viewer.set_write_timeout(timeout).unwrap();
        write_message(
            &mut viewer,
            &RelayRequest::Claim {
                version: PROTOCOL_VERSION,
                route_id,
            },
        )
        .expect("claim host");
        assert_eq!(
            read_message::<_, RelayResponse>(&mut host).expect("host paired"),
            RelayResponse::ClaimAccepted
        );
        assert_eq!(
            read_message::<_, RelayResponse>(&mut viewer).expect("viewer paired"),
            RelayResponse::ClaimAccepted
        );

        host.write_all(b"opaque-host-frame").expect("host write");
        let mut from_host = [0_u8; 17];
        viewer.read_exact(&mut from_host).expect("viewer read");
        assert_eq!(&from_host, b"opaque-host-frame");

        viewer.write_all(b"opaque-input").expect("viewer write");
        let mut from_viewer = [0_u8; 12];
        host.read_exact(&mut from_viewer).expect("host read");
        assert_eq!(&from_viewer, b"opaque-input");

        let _ = host.shutdown(Shutdown::Both);
        let _ = viewer.shutdown(Shutdown::Both);
        server.join().expect("relay server");
        assert_eq!(total.load(Ordering::Acquire), 0);
        assert!(quotas.lock().expect("quota mutex").is_empty());
    }

    #[test]
    fn per_ip_quota_is_held_until_permits_are_dropped() {
        let total = Arc::new(AtomicUsize::new(0));
        let quotas = ConnectionQuotas::default();
        let first_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let second_ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2));

        let mut permits = (0..MAX_CONNECTIONS_PER_IP)
            .map(|_| {
                ConnectionPermit::try_acquire(&total, &quotas, first_ip)
                    .expect("permit within per-IP quota")
            })
            .collect::<Vec<_>>();
        assert!(ConnectionPermit::try_acquire(&total, &quotas, first_ip).is_none());
        assert!(ConnectionPermit::try_acquire(&total, &quotas, second_ip).is_some());

        permits.pop();
        assert!(ConnectionPermit::try_acquire(&total, &quotas, first_ip).is_some());
    }

    #[test]
    fn total_connection_quota_is_enforced_across_source_ips() {
        let total = Arc::new(AtomicUsize::new(0));
        let quotas = ConnectionQuotas::default();
        let permits = (1..=MAX_CONNECTIONS)
            .map(|index| {
                let address = IpAddr::V6(Ipv6Addr::from(index as u128));
                ConnectionPermit::try_acquire(&total, &quotas, address)
                    .expect("permit within total quota")
            })
            .collect::<Vec<_>>();

        assert!(
            ConnectionPermit::try_acquire(
                &total,
                &quotas,
                IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
            )
            .is_none()
        );
        drop(permits);
        assert_eq!(total.load(Ordering::Acquire), 0);
    }

    #[test]
    fn initial_request_has_an_absolute_deadline() {
        let (mut client, mut server) = tcp_pair();
        client.write_all(&[0]).expect("write partial prefix");

        let started = Instant::now();
        let error = read_relay_request(&mut server, Duration::from_millis(50))
            .expect_err("partial request must time out");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(error.chain().any(|cause| {
            cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::TimedOut)
        }));
    }

    #[test]
    fn disconnected_host_registration_is_removed_early() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("local address");
        let waiting = WaitingHosts::default();
        let server_waiting = Arc::clone(&waiting);
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            handle_connection(stream, &server_waiting, None).expect("handle connection");
        });

        let route_id = [9_u8; 10];
        let mut host = TcpStream::connect(address).expect("connect host");
        host.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        write_message(
            &mut host,
            &RelayRequest::Register {
                version: PROTOCOL_VERSION,
                route_id,
            },
        )
        .expect("register host");
        assert!(matches!(
            read_message::<_, RelayResponse>(&mut host).expect("registered"),
            RelayResponse::Registered { .. }
        ));
        assert!(
            waiting
                .lock()
                .expect("waiting mutex")
                .contains_key(&route_id)
        );
        drop(host);

        let deadline = Instant::now() + Duration::from_secs(3);
        while waiting
            .lock()
            .expect("waiting mutex")
            .contains_key(&route_id)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !waiting
                .lock()
                .expect("waiting mutex")
                .contains_key(&route_id)
        );
        server.join().expect("relay server");
    }

    #[test]
    fn pending_host_data_is_rejected_and_route_is_removed_early() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("local address");
        let waiting = WaitingHosts::default();
        let server_waiting = Arc::clone(&waiting);
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            handle_connection(stream, &server_waiting, None).expect("handle connection");
        });

        let route_id = [10_u8; 10];
        let mut host = TcpStream::connect(address).expect("connect host");
        host.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        write_message(
            &mut host,
            &RelayRequest::Register {
                version: PROTOCOL_VERSION,
                route_id,
            },
        )
        .expect("register host");
        assert!(matches!(
            read_message::<_, RelayResponse>(&mut host).expect("registered"),
            RelayResponse::Registered { .. }
        ));
        host.write_all(b"unexpected-before-pairing")
            .expect("write protocol violation");
        drop(host);

        let deadline = Instant::now() + Duration::from_secs(3);
        while waiting
            .lock()
            .expect("waiting mutex")
            .contains_key(&route_id)
            && Instant::now() < deadline
        {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !waiting
                .lock()
                .expect("waiting mutex")
                .contains_key(&route_id)
        );
        server.join().expect("relay server");
    }

    #[test]
    fn one_direction_ending_shuts_down_the_whole_tunnel() {
        let (host_client, host_relay) = tcp_pair();
        let (mut viewer_client, viewer_relay) = tcp_pair();
        viewer_client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("viewer timeout");

        let relay = thread::spawn(move || proxy(host_relay, viewer_relay).expect("proxy"));
        host_client
            .shutdown(Shutdown::Write)
            .expect("half-close host");

        let mut byte = [0_u8; 1];
        let read = viewer_client.read(&mut byte);
        assert!(
            matches!(read, Ok(0))
                || matches!(
                    read,
                    Err(error)
                        if matches!(
                            error.kind(),
                            std::io::ErrorKind::ConnectionReset
                                | std::io::ErrorKind::ConnectionAborted
                        )
                )
        );
        relay.join().expect("relay thread");
    }
}
