//! Generated certificates and bounded loopback peers; never install roots or contact remote hosts.
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ProtocolVersion, ServerConfig, ServerConnection, StreamOwned};

#[derive(Clone, Copy)]
pub enum Invalid {
    None,
    Expired,
    Future,
    ClientOnly,
    Signature,
}

pub struct Identity {
    pub root: Vec<u8>,
    leaf: Vec<u8>,
    key: Vec<u8>,
}

static NEXT_AUTHORITY: AtomicUsize = AtomicUsize::new(1);

impl Identity {
    pub fn new(name: &str, invalid: Invalid) -> Self {
        let ca_key = KeyPair::generate().expect("CA key");
        let mut ca = CertificateParams::new(Vec::<String>::new()).expect("CA parameters");
        ca.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        // Distinct names make an unrelated CA test exercise missing trust, not a
        // failed signature under a different key with an accidentally identical subject.
        ca.distinguished_name.push(
            DnType::CommonName,
            format!(
                "NBReq portable CA {}",
                NEXT_AUTHORITY.fetch_add(1, Ordering::Relaxed)
            ),
        );
        ca.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
        let ca = ca.self_signed(&ca_key).expect("CA certificate");
        let key = KeyPair::generate().expect("leaf key");
        let mut leaf = CertificateParams::new(vec![name.to_owned()]).expect("leaf parameters");
        let now = time::OffsetDateTime::now_utc();
        leaf.not_before = now - time::Duration::days(1);
        leaf.not_after = now + time::Duration::days(30);
        leaf.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        match invalid {
            Invalid::Expired => {
                leaf.not_before = now - time::Duration::days(30);
                leaf.not_after = now - time::Duration::days(1);
            }
            Invalid::Future => {
                leaf.not_before = now + time::Duration::days(1);
                leaf.not_after = now + time::Duration::days(30);
            }
            Invalid::ClientOnly => {
                leaf.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth]
            }
            Invalid::None | Invalid::Signature => {}
        }
        let leaf = leaf.signed_by(&key, &ca, &ca_key).expect("signed leaf");
        let mut leaf = leaf.der().to_vec();
        if matches!(invalid, Invalid::Signature) {
            // Corrupt the signature value only; retain DER shape and the matching leaf public key.
            *leaf.last_mut().expect("signature byte") ^= 1;
        }
        Self {
            root: ca.der().to_vec(),
            leaf,
            key: key.serialize_der(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Protocol {
    Tls12,
    Tls13,
}
pub enum Service {
    Http(Option<String>),
    Direct,
    StartTls,
}

pub struct Peer {
    pub address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<Result<ProtocolVersion, String>>>,
}

impl Peer {
    pub fn spawn(identity: &Identity, protocol: Protocol, service: Service) -> Self {
        let version = match protocol {
            Protocol::Tls12 => &rustls::version::TLS12,
            Protocol::Tls13 => &rustls::version::TLS13,
        };
        let server =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_protocol_versions(&[version])
                .expect("protocol")
                .with_no_client_auth()
                .with_single_cert(
                    vec![CertificateDer::from(identity.leaf.clone())],
                    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key.clone())),
                )
                .expect("server identity");
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        listener.set_nonblocking(true).expect("bounded accept");
        let address = listener.local_addr().expect("fixture address");
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let worker = thread::spawn(move || -> Result<ProtocolVersion, String> {
            let deadline = Instant::now() + Duration::from_secs(6);
            let mut socket = loop {
                if worker_stop.load(Ordering::Acquire) || Instant::now() > deadline {
                    return Err("fixture stopped before accept".into());
                }
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => return Err(error.to_string()),
                }
            };
            socket.set_nonblocking(false).map_err(|e| e.to_string())?;
            socket
                .set_read_timeout(Some(Duration::from_secs(4)))
                .map_err(|e| e.to_string())?;
            socket
                .set_write_timeout(Some(Duration::from_secs(4)))
                .map_err(|e| e.to_string())?;
            if matches!(service, Service::StartTls) {
                let mut command = [0; 10];
                socket.read_exact(&mut command).map_err(|e| e.to_string())?;
                if &command != b"STARTTLS\r\n" {
                    return Err("unexpected upgrade command".into());
                }
                socket
                    .write_all(b"220 Ready\r\n")
                    .map_err(|e| e.to_string())?;
            }
            let connection = ServerConnection::new(Arc::new(server)).map_err(|e| e.to_string())?;
            let mut stream = StreamOwned::new(connection, socket);
            match service {
                Service::Http(location) => {
                    let mut head = Vec::new();
                    while !head.ends_with(b"\r\n\r\n") {
                        if head.len() > 8192 || Instant::now() > deadline {
                            return Err("HTTP fixture request bound".into());
                        }
                        let mut byte = [0];
                        stream.read_exact(&mut byte).map_err(|e| e.to_string())?;
                        head.push(byte[0]);
                    }
                    let response = if let Some(location) = location {
                        format!(
                            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                    } else {
                        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".into()
                    };
                    stream
                        .write_all(response.as_bytes())
                        .map_err(|e| e.to_string())?;
                }
                Service::Direct | Service::StartTls => {
                    let mut bytes = [0; 4];
                    stream.read_exact(&mut bytes).map_err(|e| e.to_string())?;
                    if &bytes != b"ping" {
                        return Err("unexpected encrypted application bytes".into());
                    }
                    stream.write_all(b"pong").map_err(|e| e.to_string())?;
                }
            }
            stream.flush().map_err(|e| e.to_string())?;
            let version = stream
                .conn
                .protocol_version()
                .ok_or("no negotiated protocol")?;
            stream.conn.send_close_notify();
            // The client may already have consumed its response and closed; success is the
            // authenticated application exchange, not receipt of our final close notification.
            let _ = stream.flush();
            Ok(version)
        });
        Self {
            address,
            stop,
            worker: Some(worker),
        }
    }

    pub fn assert_completed(mut self, protocol: Protocol) {
        let actual = self
            .worker
            .take()
            .expect("fixture worker")
            .join()
            .expect("fixture panic")
            .expect("fixture exchange");
        let expected = match protocol {
            Protocol::Tls12 => ProtocolVersion::TLSv1_2,
            Protocol::Tls13 => ProtocolVersion::TLSv1_3,
        };
        assert_eq!(actual, expected);
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join().expect("fixture worker panic");
        }
    }
}
