//! A small local TLS peer for the C04/C05 examples, not an SMTP or IMAP implementation.
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

pub const MESSAGE: &[u8] = b"hello over verified TLS\n";
const DEADLINE: Duration = Duration::from_secs(20);

#[derive(Clone, Copy)]
// This shared helper is compiled separately for each example, which uses one variant.
#[allow(dead_code)]
pub enum Mode {
    Immediate,
    StartTls,
}

pub struct Server {
    address: SocketAddr,
    root_der: Vec<u8>,
    stop: Arc<AtomicBool>,
    active: Arc<Mutex<Option<TcpStream>>>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

// Each socket operation rechecks the whole-fixture deadline. A peer that trickles bytes cannot
// keep a read_exact call alive forever; Server::join also shuts down an accepted socket.
struct DeadlineSocket {
    socket: TcpStream,
    stop: Arc<AtomicBool>,
    deadline: Instant,
}

impl DeadlineSocket {
    fn remaining(&self) -> io::Result<Duration> {
        if self.stop.load(Ordering::Acquire) {
            // read_exact/write_all retry Interrupted, so cancellation must be terminal.
            return Err(io::ErrorKind::ConnectionAborted.into());
        }
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(io::ErrorKind::TimedOut.into());
        }
        Ok(remaining.min(Duration::from_secs(5)))
    }
}

impl Read for DeadlineSocket {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.socket.set_read_timeout(Some(self.remaining()?))?;
        self.socket.read(buffer)
    }
}

impl Write for DeadlineSocket {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.socket.set_write_timeout(Some(self.remaining()?))?;
        self.socket.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.remaining()?;
        self.socket.flush()
    }
}

impl Server {
    pub fn start(mode: Mode) -> io::Result<Self> {
        let ca_key = KeyPair::generate().map_err(io::Error::other)?;
        let mut ca_params =
            CertificateParams::new(Vec::<String>::new()).map_err(io::Error::other)?;
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params
            .distinguished_name
            .push(DnType::CommonName, "NBReq example private CA");
        ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca = ca_params.self_signed(&ca_key).map_err(io::Error::other)?;
        let key = KeyPair::generate().map_err(io::Error::other)?;
        let mut params =
            CertificateParams::new(vec!["127.0.0.1".to_owned()]).map_err(io::Error::other)?;
        params
            .distinguished_name
            .push(DnType::CommonName, "127.0.0.1");
        params.use_authority_key_identifier_extension = true;
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        let leaf = params
            .signed_by(&key, &ca, &ca_key)
            .map_err(io::Error::other)?;
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[&rustls::version::TLS13])
                .map_err(io::Error::other)?
                .with_no_client_auth()
                .with_single_cert(
                    vec![leaf.der().clone()],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
                )
                .map_err(io::Error::other)?;
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let active = Arc::new(Mutex::new(None));
        let worker_stop = Arc::clone(&stop);
        let worker_active = Arc::clone(&active);
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + DEADLINE;
            let socket = loop {
                if worker_stop.load(Ordering::Acquire) {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => return Err(error),
                }
            };
            // Accepted sockets may inherit nonblocking mode on Windows.
            socket.set_nonblocking(false)?;
            *worker_active
                .lock()
                .map_err(|_| io::Error::other("socket lock poisoned"))? = Some(socket.try_clone()?);
            if worker_stop.load(Ordering::Acquire) {
                return Ok(());
            }
            let mut socket = DeadlineSocket {
                socket,
                stop: worker_stop,
                deadline,
            };
            if matches!(mode, Mode::StartTls) {
                negotiate_starttls(&mut socket)?;
            }
            let connection = ServerConnection::new(Arc::new(config)).map_err(io::Error::other)?;
            let mut tls = StreamOwned::new(connection, socket);
            let mut received = vec![0_u8; MESSAGE.len()];
            tls.read_exact(&mut received)?;
            if received != MESSAGE {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unexpected protected message",
                ));
            }
            tls.write_all(&received)?;
            tls.flush()?;
            tls.conn.send_close_notify();
            while tls.conn.wants_write() {
                tls.conn.write_tls(&mut tls.sock)?;
            }
            Ok(())
        });
        Ok(Self {
            address,
            root_der: ca.der().to_vec(),
            stop,
            active,
            worker: Some(worker),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn root_der(&self) -> &[u8] {
        &self.root_der
    }

    pub fn stop(mut self) -> io::Result<()> {
        self.join()
    }

    fn join(&mut self) -> io::Result<()> {
        self.stop.store(true, Ordering::Release);
        if let Ok(active) = self.active.lock() {
            if let Some(socket) = active.as_ref() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("local TLS server panicked"))??;
        }
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.join();
    }
}

fn negotiate_starttls(socket: &mut DeadlineSocket) -> io::Result<()> {
    socket.write_all(b"220 local.example ready\r\n")?;
    let mut command = Vec::new();
    while command.len() < 128 {
        let mut byte = [0_u8; 1];
        socket.read_exact(&mut byte)?;
        command.push(byte[0]);
        if command.ends_with(b"\r\n") {
            break;
        }
    }
    if !command.eq_ignore_ascii_case(b"STARTTLS\r\n") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected STARTTLS command",
        ));
    }
    socket.write_all(b"220 Ready to start TLS\r\n")?;
    Ok(())
}
