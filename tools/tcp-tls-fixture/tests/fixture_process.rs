//! Independent process-level proof of the measurement fixture's TLS and lifecycle contract.
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, ClientConnection, RootCertStore};
use serde_json::Value;
use tempfile::TempDir;

struct Fixture {
    child: Child,
    folder: TempDir,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Fixture {
    fn start(count: usize, version: &str) -> Self {
        let folder = tempfile::tempdir().unwrap();
        let log = fs::File::create(folder.path().join("fixture.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_tcp-tls-fixture"))
            .arg("--expected")
            .arg(count.to_string())
            .args(["--tls-version", version, "--ca-der"])
            .arg(folder.path().join("root.der"))
            .arg("--ready")
            .arg(folder.path().join("ready.json"))
            .arg("--report")
            .arg(folder.path().join("report.json"))
            .stdin(Stdio::piped())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        Self { child, folder }
    }

    fn ready(&mut self) -> Value {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Ok(bytes) = fs::read(self.folder.path().join("ready.json")) {
                let ready: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(ready["pid"].as_u64(), Some(u64::from(self.child.id())));
                let address: SocketAddr = ready["address"].as_str().unwrap().parse().unwrap();
                assert!(address.is_ipv4() && address.ip().is_loopback());
                assert_ne!(address.port(), 0);
                return ready;
            }
            assert!(
                self.child.try_wait().unwrap().is_none(),
                "fixture exited before readiness"
            );
            assert!(Instant::now() < deadline, "fixture did not become ready");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn terminal(&mut self, passed: bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert_eq!(
                    status.success(),
                    passed,
                    "{}",
                    fs::read_to_string(self.folder.path().join("fixture.log")).unwrap()
                );
                let bytes = fs::read(self.folder.path().join("report.json")).unwrap();
                let report: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(report["status"], if passed { "passed" } else { "failed" });
                return report;
            }
            assert!(Instant::now() < deadline, "fixture did not terminate");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn stop(&mut self) {
        self.child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"STOP\n")
            .unwrap();
        self.child.stdin.as_mut().unwrap().flush().unwrap();
    }
}

fn config(root: Option<Vec<u8>>, version: &str) -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    if let Some(root) = root {
        roots.add(CertificateDer::from(root)).unwrap();
    }
    let version = if version == "1.2" {
        &rustls::version::TLS12
    } else {
        &rustls::version::TLS13
    };
    Arc::new(
        ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_protocol_versions(&[version])
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

fn connect(
    address: &str,
    config: Arc<ClientConfig>,
) -> Result<(ClientConnection, TcpStream), String> {
    let mut socket = TcpStream::connect(address).map_err(|e| e.to_string())?;
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    socket
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut client =
        ClientConnection::new(config, ServerName::try_from("127.0.0.1").unwrap()).unwrap();
    while client.is_handshaking() || client.wants_write() {
        client.complete_io(&mut socket).map_err(|e| e.to_string())?;
    }
    Ok((client, socket))
}

#[test]
fn verified_tls12_sixteen_connections_have_exact_report_and_release_listener() {
    successful_hold("1.2", 16);
}

#[test]
fn verified_tls13_thirty_two_connections_have_exact_report_and_release_listener() {
    successful_hold("1.3", 32);
}

fn successful_hold(version: &str, count: usize) {
    let mut fixture = Fixture::start(count, version);
    let ready = fixture.ready();
    let address = ready["address"].as_str().unwrap();
    let root = fs::read(fixture.folder.path().join("root.der")).unwrap();
    let cfg = config(Some(root), version);
    let mut sessions = Vec::new();
    for _ in 0..count {
        let session = connect(address, cfg.clone()).expect("fixture must complete verified TLS");
        let expected = if version == "1.2" {
            rustls::ProtocolVersion::TLSv1_2
        } else {
            rustls::ProtocolVersion::TLSv1_3
        };
        assert_eq!(session.0.protocol_version(), Some(expected));
        sessions.push(session);
    }
    // Client Finished has been flushed; permit the fixture's bounded polling loop to consume it.
    thread::sleep(Duration::from_millis(100));
    assert!(fixture.child.try_wait().unwrap().is_none());
    // Drain post-handshake TLS records so queued tickets cannot hide an early FIN.
    for (client, socket) in &mut sessions {
        socket.set_nonblocking(true).unwrap();
        loop {
            match client.read_tls(socket) {
                Ok(0) => panic!("fixture closed an accepted socket before STOP"),
                Ok(_) => assert!(
                    !client.process_new_packets().unwrap().peer_has_closed(),
                    "fixture sent close_notify before STOP"
                ),
                Err(error) if error.kind() == ErrorKind::WouldBlock => break,
                Err(error) => panic!("held socket failed before STOP: {error}"),
            }
        }
    }
    fixture.stop();
    let report = fixture.terminal(true);
    assert_eq!(report["accepted"], count);
    assert_eq!(report["handshakes"], count);
    assert_eq!(report["expected"], count);
    assert_eq!(report["pid"], fixture.child.id());
    assert_eq!(report["address"], ready["address"]);
    let versions = report["negotiated_versions"].as_array().unwrap();
    assert_eq!(versions.len(), count);
    for actual in versions {
        assert_eq!(actual, &format!("TLSv{version}"));
    }
    assert!(
        TcpStream::connect_timeout(&address.parse().unwrap(), Duration::from_millis(250)).is_err()
    );
    for (_, socket) in &mut sessions {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut buffer = [0_u8; 4096];
        loop {
            match socket.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::ConnectionReset
                            | ErrorKind::ConnectionAborted
                            | ErrorKind::BrokenPipe
                    ) =>
                {
                    break;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Err(error) => panic!("unexpected socket-close result: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "accepted socket remained open after terminal success"
            );
            thread::sleep(Duration::from_millis(5));
        }
    }
    for item in fs::read_dir(fixture.folder.path()).unwrap() {
        let name = item.unwrap().file_name();
        assert!(
            matches!(
                name.to_str().unwrap(),
                "root.der" | "ready.json" | "report.json" | "fixture.log"
            ),
            "unexpected persisted file {name:?}"
        );
    }
}

#[test]
fn untrusted_root_fails_handshake_and_cannot_report_success() {
    let mut fixture = Fixture::start(16, "1.3");
    let ready = fixture.ready();
    assert!(connect(ready["address"].as_str().unwrap(), config(None, "1.3")).is_err());
    let report = fixture.terminal(false);
    assert_eq!(report["handshakes"], 0);
}

#[test]
fn incompatible_protocol_is_rejected_without_downgrade() {
    let mut fixture = Fixture::start(16, "1.2");
    let ready = fixture.ready();
    let root = fs::read(fixture.folder.path().join("root.der")).unwrap();
    assert!(
        connect(
            ready["address"].as_str().unwrap(),
            config(Some(root), "1.3")
        )
        .is_err()
    );
    assert_eq!(fixture.terminal(false)["handshakes"], 0);
}

#[test]
fn early_stop_fails_exact_count_and_closes_listener() {
    let mut fixture = Fixture::start(16, "1.3");
    let ready = fixture.ready();
    fixture.stop();
    assert_eq!(fixture.terminal(false)["handshakes"], 0);
    assert!(TcpStream::connect(ready["address"].as_str().unwrap()).is_err());
}

#[test]
fn slow_handshake_bytes_cannot_extend_absolute_deadline() {
    let mut fixture = Fixture::start(16, "1.3");
    let ready = fixture.ready();
    let mut socket = TcpStream::connect(ready["address"].as_str().unwrap()).unwrap();
    let mut hello = Vec::new();
    ClientConnection::new(
        config(None, "1.3"),
        ServerName::try_from("127.0.0.1").unwrap(),
    )
    .unwrap()
    .write_tls(&mut hello)
    .unwrap();
    let started = Instant::now();
    for byte in hello {
        if fixture.child.try_wait().unwrap().is_some() {
            break;
        }
        if socket.write_all(&[byte]).is_err() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "trickled bytes extended handshake deadline"
        );
        thread::sleep(Duration::from_millis(80));
    }
    assert_eq!(fixture.terminal(false)["handshakes"], 0);
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn invalid_count_and_existing_output_are_rejected_before_listening() {
    let mut fixture = Fixture::start(1, "1.3");
    let deadline = Instant::now() + Duration::from_secs(3);
    while fixture.child.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!fixture.child.wait().unwrap().success());
    assert!(!fixture.folder.path().join("ready.json").exists());
    assert!(!fixture.folder.path().join("root.der").exists());
    let root = fixture.folder.path().join("existing.der");
    fs::write(&root, b"preserve me").unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_tcp-tls-fixture"))
        .args(["--expected", "16", "--tls-version", "1.3", "--ca-der"])
        .arg(&root)
        .arg("--ready")
        .arg(fixture.folder.path().join("ready.json"))
        .arg("--report")
        .arg(fixture.folder.path().join("report.json"))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(!status.success());
    assert_eq!(fs::read(root).unwrap(), b"preserve me");
}
