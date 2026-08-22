use std::io::{ErrorKind, Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use nbreq::{Completion, Engine, EngineConfig, Request};

#[derive(Clone, Copy)]
enum Backend {
    Native,
    Curl,
}

#[derive(Clone, Copy)]
enum Stall {
    Headers,
    Body,
}

fn build_engine(backend: Backend) -> Result<Engine, nbreq::Error> {
    let config = EngineConfig::spawned();
    match backend {
        Backend::Native => nbreq::testing::native_http_engine(config),
        Backend::Curl => nbreq::testing::curl_engine(config),
    }
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn percentile(sorted: &[Duration], numerator: usize) -> Duration {
    sorted[(sorted.len() - 1) * numerator / 100]
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let backend_name = arguments.next().unwrap_or_else(|| "native".to_owned());
    let backend = match backend_name.as_str() {
        "native" => Backend::Native,
        "curl" => Backend::Curl,
        _ => return Err("backend must be 'native' or 'curl'".into()),
    };
    let stall_name = arguments.next().unwrap_or_else(|| "headers".to_owned());
    let stall = match stall_name.as_str() {
        "headers" => Stall::Headers,
        "body" => Stall::Body,
        _ => return Err("stall must be 'headers' or 'body'".into()),
    };
    let trials = arguments
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(100);
    if trials == 0 {
        return Err("trial count must be greater than zero".into());
    }

    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let (ready_tx, ready_rx) = mpsc::channel();
    let (closed_tx, closed_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        for trial in 0..trials {
            let (mut stream, _) = listener.accept().expect("cancellation fixture must accept");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("cancellation fixture timeout must configure");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream
                    .read(&mut buffer)
                    .expect("cancellation fixture request must read");
                assert_ne!(read, 0, "client closed before cancellation barrier");
                request.extend_from_slice(&buffer[..read]);
            }
            if matches!(stall, Stall::Body) {
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nabc")
                    .expect("partial cancellation response must write");
                stream
                    .flush()
                    .expect("partial cancellation response must flush");
            }
            ready_tx
                .send(trial)
                .expect("cancellation barrier must signal");
            loop {
                match stream.read(&mut buffer) {
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
                    Err(error) => panic!("cancelled socket did not close promptly: {error}"),
                }
            }
            closed_tx
                .send((trial, Instant::now()))
                .expect("peer-close observation must send");
        }
    });

    let engine = build_engine(backend)?;
    let client = engine.client();
    let mut terminal_latencies = Vec::with_capacity(trials);
    let mut peer_latencies = Vec::with_capacity(trials);
    for trial in 0..trials {
        let pending = client.submit(
            Request::get(format!("http://{address}/trial-{trial}"))
                .total_timeout(Duration::from_secs(5))
                .build()?,
        )?;
        if ready_rx.recv_timeout(Duration::from_secs(2))? != trial {
            return Err("cancellation barrier arrived out of order".into());
        }
        let started = Instant::now();
        pending.handle().cancel()?;
        if !matches!(pending.wait(), Completion::Cancelled) {
            return Err("cancellation lost the terminal race".into());
        }
        terminal_latencies.push(started.elapsed());
        let (closed_trial, closed_at) = closed_rx.recv_timeout(Duration::from_secs(2))?;
        if closed_trial != trial {
            return Err("peer close arrived out of order".into());
        }
        peer_latencies.push(closed_at.saturating_duration_since(started));
    }
    engine.shutdown()?;
    server
        .join()
        .map_err(|_| "cancellation fixture thread panicked")?;
    terminal_latencies.sort_unstable();
    peer_latencies.sort_unstable();

    println!("backend={backend_name}");
    println!("stall={stall_name}");
    println!("trials={trials}");
    println!(
        "terminal_median_ms={:.3}",
        milliseconds(percentile(&terminal_latencies, 50))
    );
    println!(
        "terminal_p95_ms={:.3}",
        milliseconds(percentile(&terminal_latencies, 95))
    );
    println!(
        "terminal_max_ms={:.3}",
        milliseconds(*terminal_latencies.last().expect("trials are nonempty"))
    );
    println!(
        "peer_median_ms={:.3}",
        milliseconds(percentile(&peer_latencies, 50))
    );
    println!(
        "peer_p95_ms={:.3}",
        milliseconds(percentile(&peer_latencies, 95))
    );
    println!(
        "peer_max_ms={:.3}",
        milliseconds(*peer_latencies.last().expect("trials are nonempty"))
    );
    Ok(())
}
