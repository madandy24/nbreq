use std::alloc::System;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nbreq::{Engine, EngineConfig, Request};
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, Stats, StatsAlloc};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[derive(Clone, Copy)]
enum Backend {
    Native,
    Curl,
}

struct LocalServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    connections: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
    joined: Option<JoinHandle<()>>,
}

impl LocalServer {
    fn spawn(body_bytes: usize) -> Result<Self, Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let connections = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
        let response = Arc::new(
            [
                format!("HTTP/1.1 200 OK\r\nContent-Length: {body_bytes}\r\n\r\n").into_bytes(),
                vec![b'x'; body_bytes],
            ]
            .concat(),
        );
        let thread_stop = Arc::clone(&stop);
        let thread_connections = Arc::clone(&connections);
        let thread_requests = Arc::clone(&requests);
        let joined = thread::spawn(move || {
            let mut handlers = Vec::new();
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        thread_connections.fetch_add(1, Ordering::AcqRel);
                        handlers.push(spawn_connection(
                            stream,
                            Arc::clone(&response),
                            Arc::clone(&thread_requests),
                            Arc::clone(&thread_stop),
                        ));
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("comparison server accept failed: {error}"),
                }
            }
            for handler in handlers {
                handler
                    .join()
                    .expect("comparison connection handler must join");
            }
        });
        Ok(Self {
            address,
            stop,
            connections,
            requests,
            joined: Some(joined),
        })
    }

    fn stop(mut self) -> Result<(usize, usize), Box<dyn std::error::Error>> {
        self.stop.store(true, Ordering::Release);
        if let Some(joined) = self.joined.take() {
            joined
                .join()
                .map_err(|_| "comparison server thread panicked")?;
        }
        Ok((
            self.connections.load(Ordering::Acquire),
            self.requests.load(Ordering::Acquire),
        ))
    }
}

fn spawn_connection(
    mut stream: TcpStream,
    response: Arc<Vec<u8>>,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        stream
            .set_read_timeout(Some(Duration::from_millis(25)))
            .expect("comparison connection timeout must configure");
        let mut request = [0_u8; 8192];
        let mut used = 0;
        while !stop.load(Ordering::Acquire) {
            match stream.read(&mut request[used..]) {
                Ok(0) => return,
                Ok(read) => {
                    used += read;
                    if request[..used]
                        .windows(4)
                        .any(|window| window == b"\r\n\r\n")
                    {
                        requests.fetch_add(1, Ordering::AcqRel);
                        if stream.write_all(&response).is_err() {
                            return;
                        }
                        used = 0;
                    } else if used == request.len() {
                        panic!("comparison request head exceeded fixture storage");
                    }
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(_) => return,
            }
        }
    })
}

fn build_engine(backend: Backend) -> Result<Engine, nbreq::Error> {
    let config = EngineConfig::spawned();
    match backend {
        Backend::Native => nbreq::testing::native_http_engine(config),
        Backend::Curl => nbreq::testing::curl_engine(config),
    }
}

fn net_bytes(stats: &Stats) -> i128 {
    // stats_alloc includes growth and shrinkage from realloc in the allocated/deallocated totals;
    // bytes_reallocated is useful diagnostic detail but must not be added a second time.
    stats.bytes_allocated as i128 - stats.bytes_deallocated as i128
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    let backend_name = arguments.next().unwrap_or_else(|| "native".to_owned());
    let backend = match backend_name.as_str() {
        "native" => Backend::Native,
        "curl" => Backend::Curl,
        _ => return Err("backend must be 'native' or 'curl'".into()),
    };
    let request_count = arguments
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(10_000);
    let body_bytes = arguments
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()?
        .unwrap_or(128);
    if request_count == 0 {
        return Err("request count must be greater than zero".into());
    }

    let server = LocalServer::spawn(body_bytes)?;
    let url = format!("http://{}/comparison", server.address);
    let lifecycle = Region::new(GLOBAL);
    let engine = build_engine(backend)?;
    let client = engine.client();
    let warm = client.execute(
        Request::get(url.clone())
            .total_timeout(Duration::from_secs(5))
            .build()?,
    )?;
    if warm.body().len() != body_bytes {
        return Err("warm response body length did not match".into());
    }
    drop(warm);
    thread::sleep(Duration::from_millis(25));

    let workload = Region::new(GLOBAL);
    let started = Instant::now();
    for _ in 0..request_count {
        let response = client.execute(
            Request::get(url.clone())
                .total_timeout(Duration::from_secs(5))
                .build()?,
        )?;
        if response.body().len() != body_bytes {
            return Err("measured response body length did not match".into());
        }
    }
    let elapsed = started.elapsed();
    thread::sleep(Duration::from_millis(25));
    let workload_stats = workload.change();
    let idle_stats = lifecycle.change();
    let engine_metrics = engine.metrics();
    engine.shutdown()?;
    let shutdown_stats = lifecycle.change();
    let (server_connections, server_requests) = server.stop()?;
    let seconds = elapsed.as_secs_f64();

    println!("backend={backend_name}");
    println!("requests={request_count}");
    println!("body_bytes={body_bytes}");
    println!("elapsed_ms={:.3}", seconds * 1000.0);
    println!("requests_per_second={:.3}", request_count as f64 / seconds);
    println!("allocations={}", workload_stats.allocations);
    println!("deallocations={}", workload_stats.deallocations);
    println!("reallocations={}", workload_stats.reallocations);
    println!("bytes_allocated={}", workload_stats.bytes_allocated);
    println!("bytes_deallocated={}", workload_stats.bytes_deallocated);
    println!("bytes_reallocated={}", workload_stats.bytes_reallocated);
    println!("workload_net_bytes={}", net_bytes(&workload_stats));
    println!("idle_lifecycle_net_bytes={}", net_bytes(&idle_stats));
    println!("post_shutdown_net_bytes={}", net_bytes(&shutdown_stats));
    println!("server_connections={server_connections}");
    println!("server_requests={server_requests}");
    println!(
        "native_connections_opened={}",
        engine_metrics.connections_opened()
    );
    println!(
        "native_connections_reused={}",
        engine_metrics.connections_reused()
    );
    println!(
        "native_connections_closed={}",
        engine_metrics.connections_closed()
    );
    Ok(())
}
