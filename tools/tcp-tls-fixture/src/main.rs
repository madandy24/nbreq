//! Bounded loopback TLS memory-observation fixture.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ProtocolVersion, ServerConfig, ServerConnection};
use serde_json::json;

#[derive(Clone, Debug)]
struct Options {
    expected: usize,
    tls_version: String,
    ca_der: PathBuf,
    ready: PathBuf,
    report: PathBuf,
}

fn parse_args() -> Result<Options, String> {
    let mut args = std::env::args().skip(1);
    let mut expected = None;
    let mut tls_version = None;
    let mut ca_der = None;
    let mut ready = None;
    let mut report = None;
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--expected" => expected = Some(value.parse::<usize>().map_err(|_| "invalid count")?),
            "--tls-version" => tls_version = Some(value),
            "--ca-der" => ca_der = Some(PathBuf::from(value)),
            "--ready" => ready = Some(PathBuf::from(value)),
            "--report" => report = Some(PathBuf::from(value)),
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    let expected = expected.ok_or("missing --expected")?;
    if !matches!(expected, 16 | 32) {
        return Err("expected count must be 16 or 32".into());
    }
    let tls_version = tls_version.ok_or("missing --tls-version")?;
    if !matches!(tls_version.as_str(), "1.2" | "1.3") {
        return Err("TLS version must be 1.2 or 1.3".into());
    }
    let ca_der = ca_der.ok_or("missing --ca-der")?;
    let ready = ready.ok_or("missing --ready")?;
    let report = report.ok_or("missing --report")?;
    if ca_der == ready || ca_der == report || ready == report {
        return Err("fixture output paths must be distinct".into());
    }
    if [&ca_der, &ready, &report].iter().any(|path| path.exists()) {
        return Err("fixture output paths must be new".into());
    }
    Ok(Options {
        expected,
        tls_version,
        ca_der,
        ready,
        report,
    })
}

fn publish(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut temporary = path.to_path_buf();
    temporary.set_extension(format!("{}.tmp", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::hard_link(&temporary, path)?;
    fs::remove_file(temporary)?;
    Ok(())
}

fn read_control() -> Result<(), String> {
    let mut stdin = io::stdin().lock();
    let mut bytes = [0u8; 16];
    for index in 0..bytes.len() {
        stdin
            .read_exact(&mut bytes[index..index + 1])
            .map_err(|_| "control input ended before STOP")?;
        if bytes[index] == b'\n' {
            return if &bytes[..=index] == b"STOP\n" {
                Ok(())
            } else {
                Err("invalid fixture control command".into())
            };
        }
    }
    Err("fixture control command exceeded 16 bytes".into())
}

fn identity(version: &str) -> Result<(Vec<u8>, Arc<ServerConfig>), String> {
    let root_key = KeyPair::generate().map_err(|e| e.to_string())?;
    let mut root_params =
        CertificateParams::new(Vec::<String>::new()).map_err(|e| e.to_string())?;
    root_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    root_params
        .distinguished_name
        .push(DnType::CommonName, "NBReq ephemeral memory fixture CA");
    root_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let root = root_params
        .self_signed(&root_key)
        .map_err(|e| e.to_string())?;
    let leaf_key = KeyPair::generate().map_err(|e| e.to_string())?;
    let mut leaf_params =
        CertificateParams::new(vec!["127.0.0.1".into()]).map_err(|e| e.to_string())?;
    leaf_params
        .distinguished_name
        .push(DnType::CommonName, "127.0.0.1");
    leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    leaf_params.use_authority_key_identifier_extension = true;
    let now = time::OffsetDateTime::now_utc();
    leaf_params.not_before = now - time::Duration::days(1);
    leaf_params.not_after = now + time::Duration::days(30);
    let leaf = leaf_params
        .signed_by(&leaf_key, &root, &root_key)
        .map_err(|e| e.to_string())?;
    let selected = if version == "1.2" {
        &rustls::version::TLS12
    } else {
        &rustls::version::TLS13
    };
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[selected])
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_single_cert(
                vec![leaf.der().clone()],
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
            )
            .map_err(|e| e.to_string())?;
    Ok((root.der().to_vec(), Arc::new(config)))
}

fn handshake(
    socket: &mut TcpStream,
    config: Arc<ServerConfig>,
    run_deadline: Instant,
) -> Result<ProtocolVersion, String> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut connection = ServerConnection::new(config).map_err(|e| e.to_string())?;
    while connection.is_handshaking() || connection.wants_write() {
        let remaining = deadline
            .min(run_deadline)
            .saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err("TLS handshake exceeded three seconds".into());
        }
        socket
            .set_read_timeout(Some(remaining))
            .map_err(|e| e.to_string())?;
        socket
            .set_write_timeout(Some(remaining))
            .map_err(|e| e.to_string())?;
        if connection.wants_write() {
            connection
                .write_tls(&mut *socket)
                .map_err(|e| format!("TLS write: {e}"))?;
        } else {
            let received = connection
                .read_tls(&mut *socket)
                .map_err(|e| format!("TLS read: {e}"))?;
            if received == 0 {
                return Err("TLS handshake ended before completion".into());
            }
            connection
                .process_new_packets()
                .map_err(|e| format!("TLS handshake: {e}"))?;
        }
    }
    connection
        .protocol_version()
        .ok_or_else(|| "TLS version was not negotiated".into())
}

fn run(options: &Options) -> Result<(), String> {
    let started = Instant::now();
    let deadline = started + Duration::from_secs(120);
    let (ca, config) = identity(&options.tls_version)?;
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    let address = listener.local_addr().map_err(|error| error.to_string())?;
    if started.elapsed() >= Duration::from_secs(10) {
        return Err("fixture setup exceeded ten seconds".into());
    }
    publish(&options.ca_der, &ca).map_err(|error| error.to_string())?;
    let ready = json!({"pid": std::process::id(), "address": address.to_string(),
                       "expected": options.expected, "tls_version": options.tls_version});
    publish(&options.ready, ready.to_string().as_bytes()).map_err(|error| error.to_string())?;
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let (control_tx, control_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = control_tx.send(read_control());
    });
    let mut accepted = 0;
    let mut versions = Vec::new();
    let mut sessions = Vec::new();
    let outcome = loop {
        if Instant::now() >= deadline {
            break Err("fixture exceeded 120 seconds".to_owned());
        }
        if let Ok(control) = control_rx.try_recv() {
            break match control {
                Ok(()) if versions.len() == options.expected => Ok(()),
                Ok(()) => Err("STOP arrived before every TLS handshake".into()),
                Err(error) => Err(error),
            };
        }
        if accepted < options.expected {
            match listener.accept() {
                Ok((mut socket, _)) => {
                    accepted += 1;
                    if let Err(error) = socket.set_nonblocking(false) {
                        break Err(error.to_string());
                    }
                    match handshake(&mut socket, config.clone(), deadline) {
                        Ok(ProtocolVersion::TLSv1_2) => versions.push("TLSv1.2"),
                        Ok(ProtocolVersion::TLSv1_3) => versions.push("TLSv1.3"),
                        Ok(other) => break Err(format!("unexpected TLS version: {other:?}")),
                        Err(error) => {
                            eprintln!("fixture handshake {}: {error}", accepted);
                            break Err(error);
                        }
                    }
                    sessions.push(socket);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Err(error) => break Err(error.to_string()),
            }
        } else {
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    drop(listener);
    for socket in sessions {
        let _ = socket.shutdown(Shutdown::Both);
    }
    let passed =
        outcome.is_ok() && accepted == options.expected && versions.len() == options.expected;
    let report = json!({"status": if passed {"passed"} else {"failed"}, "pid":std::process::id(),
                        "address":address.to_string(), "expected":options.expected,
                        "accepted":accepted, "handshakes":versions.len(),
                        "tls_version":options.tls_version, "negotiated_versions":versions});
    publish(&options.report, report.to_string().as_bytes()).map_err(|error| error.to_string())?;
    if passed {
        Ok(())
    } else {
        Err(outcome
            .err()
            .unwrap_or_else(|| "fixture count mismatch".into()))
    }
}

fn main() {
    let result = parse_args().and_then(|options| run(&options));
    if let Err(error) = result {
        eprintln!("fixture: {error}");
        std::process::exit(1);
    }
}
