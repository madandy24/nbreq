use std::error::Error;
use std::fmt::Write as _;
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use nbreq::{Engine, EngineConfig, Request};

#[cfg(feature = "alloc-stats")]
use stats_alloc::{INSTRUMENTED_SYSTEM, Region, StatsAlloc};
#[cfg(feature = "alloc-stats")]
use std::alloc::System;

#[cfg(feature = "alloc-stats")]
#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

const SCHEMA: &str = "nbreq-f5-observation-v1";
const WORKLOAD: &str = "buffered_keepalive";

struct Arguments {
    source_commit: String,
    source_version: String,
    samples: usize,
    warmups: usize,
    requests_per_sample: usize,
    body_bytes: usize,
}

impl Arguments {
    fn parse() -> Result<Self, Box<dyn Error>> {
        let mut source_commit = None;
        let mut source_version = None;
        let mut samples = 3;
        let mut warmups = 8;
        let mut requests_per_sample = 250;
        let mut body_bytes = 128;
        let mut arguments = std::env::args().skip(1);

        while let Some(argument) = arguments.next() {
            let value = arguments
                .next()
                .ok_or_else(|| format!("missing value after {argument}"))?;
            match argument.as_str() {
                "--source-commit" => source_commit = Some(value),
                "--source-version" => source_version = Some(value),
                "--samples" => samples = value.parse()?,
                "--warmups" => warmups = value.parse()?,
                "--requests-per-sample" => requests_per_sample = value.parse()?,
                "--body-bytes" => body_bytes = value.parse()?,
                _ => return Err(format!("unknown argument: {argument}").into()),
            }
        }

        if samples == 0 || requests_per_sample == 0 {
            return Err("samples and requests per sample must be greater than zero".into());
        }
        if body_bytes == 0 || body_bytes > 8 * 1024 * 1024 {
            return Err("body bytes must be between 1 and 8388608".into());
        }

        Ok(Self {
            source_commit: source_commit.ok_or("--source-commit is required")?,
            source_version: source_version.ok_or("--source-version is required")?,
            samples,
            warmups,
            requests_per_sample,
            body_bytes,
        })
    }
}

struct LocalServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    connections: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
    joined: Option<JoinHandle<Result<(), String>>>,
}

impl LocalServer {
    fn spawn(body: Arc<Vec<u8>>) -> Result<Self, Box<dyn Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let connections = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(AtomicUsize::new(0));
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
                            Arc::clone(&body),
                            Arc::clone(&thread_requests),
                            Arc::clone(&thread_stop),
                        ));
                    }
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => return Err(format!("fixture accept failed: {error}")),
                }
            }
            for handler in handlers {
                handler
                    .join()
                    .map_err(|_| "fixture connection thread panicked".to_owned())??;
            }
            Ok(())
        });
        Ok(Self {
            address,
            stop,
            connections,
            requests,
            joined: Some(joined),
        })
    }

    fn stop(mut self) -> Result<(usize, usize), Box<dyn Error>> {
        self.stop.store(true, Ordering::Release);
        let joined = self
            .joined
            .take()
            .ok_or("fixture join handle was already consumed")?;
        joined
            .join()
            .map_err(|_| "fixture server thread panicked")?
            .map_err(|error| -> Box<dyn Error> { error.into() })?;
        Ok((
            self.connections.load(Ordering::Acquire),
            self.requests.load(Ordering::Acquire),
        ))
    }
}

fn spawn_connection(
    mut stream: TcpStream,
    body: Arc<Vec<u8>>,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<Result<(), String>> {
    thread::spawn(move || {
        stream
            .set_read_timeout(Some(Duration::from_millis(25)))
            .map_err(|error| format!("fixture timeout configuration failed: {error}"))?;
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\n\r\n",
            body.len()
        );
        let mut request = [0_u8; 8192];
        let mut used = 0;
        while !stop.load(Ordering::Acquire) {
            match stream.read(&mut request[used..]) {
                Ok(0) => return Ok(()),
                Ok(read) => {
                    used += read;
                    if request[..used]
                        .windows(4)
                        .any(|window| window == b"\r\n\r\n")
                    {
                        requests.fetch_add(1, Ordering::AcqRel);
                        stream
                            .write_all(head.as_bytes())
                            .and_then(|()| stream.write_all(&body))
                            .map_err(|error| format!("fixture response failed: {error}"))?;
                        used = 0;
                    } else if used == request.len() {
                        return Err("fixture request head exceeded 8192 bytes".to_owned());
                    }
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
                Err(error) => return Err(format!("fixture request read failed: {error}")),
            }
        }
        Ok(())
    })
}

#[derive(Clone)]
struct Summary {
    available: bool,
    unit: &'static str,
    median: f64,
    min: f64,
    max: f64,
}

impl Summary {
    fn measured(unit: &'static str, values: &[f64]) -> Result<Self, Box<dyn Error>> {
        if values.is_empty() || values.iter().any(|value| !value.is_finite()) {
            return Err("metric sample set must contain finite values".into());
        }
        let mut sorted = values.to_vec();
        sorted.sort_by(f64::total_cmp);
        let middle = sorted.len() / 2;
        let median = if sorted.len() % 2 == 0 {
            (sorted[middle - 1] + sorted[middle]) / 2.0
        } else {
            sorted[middle]
        };
        Ok(Self {
            available: true,
            unit,
            median,
            min: sorted[0],
            max: sorted[sorted.len() - 1],
        })
    }

    fn unavailable(unit: &'static str) -> Self {
        Self {
            available: false,
            unit,
            median: 0.0,
            min: 0.0,
            max: 0.0,
        }
    }

    fn push_json(&self, output: &mut String) -> Result<(), std::fmt::Error> {
        if self.available {
            write!(
                output,
                "{{\"available\":true,\"unit\":\"{}\",\"median\":{:.6},\"min\":{:.6},\"max\":{:.6}}}",
                self.unit, self.median, self.min, self.max
            )
        } else {
            write!(
                output,
                "{{\"available\":false,\"unit\":\"{}\",\"median\":null,\"min\":null,\"max\":null}}",
                self.unit
            )
        }
    }
}

struct Checks {
    bytes_exact: bool,
    connection_reuse_exact: bool,
    operation_gauges_zero: bool,
    queue_gauges_zero: bool,
    keepalive_state_exact: bool,
    joined_shutdown: bool,
    fixture_gone: bool,
}

fn expected_body(body_bytes: usize) -> Vec<u8> {
    (0..body_bytes).map(|index| (index % 251) as u8).collect()
}

fn execute_request(
    client: &nbreq::Client,
    url: &str,
    expected: &[u8],
) -> Result<(), Box<dyn Error>> {
    let response = client.execute(
        Request::get(url)
            .total_timeout(Duration::from_secs(5))
            .build()?,
    )?;
    if response.status() != 200 || response.body() != expected {
        return Err("response status or body did not match the fixture".into());
    }
    Ok(())
}

fn feature_selection() -> &'static str {
    if cfg!(feature = "resolver") {
        "default:native,resolver"
    } else {
        "no-default-features:native"
    }
}

fn json_string(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(output, "\\u{:04x}", character as u32);
            }
            character => output.push(character),
        }
    }
    output.push('"');
    output
}

fn push_metric(
    output: &mut String,
    first: &mut bool,
    name: &str,
    summary: &Summary,
) -> Result<(), std::fmt::Error> {
    if !*first {
        output.push(',');
    }
    *first = false;
    write!(output, "{}:", json_string(name))?;
    summary.push_json(output)
}

fn main() -> Result<(), Box<dyn Error>> {
    let arguments = Arguments::parse()?;
    eprintln!("F5_STAGE fixture_start");
    let body = Arc::new(expected_body(arguments.body_bytes));
    let server = LocalServer::spawn(Arc::clone(&body))?;
    let url = format!("http://{}/f5-buffered", server.address);

    eprintln!("F5_STAGE engine_start");
    let engine = Engine::new(EngineConfig::spawned())?;
    let client = engine.client();
    for _ in 0..arguments.warmups {
        execute_request(&client, &url, &body)?;
    }

    let mut wall_ms = Vec::with_capacity(arguments.samples);
    let mut requests_per_second = Vec::with_capacity(arguments.samples);
    #[cfg(feature = "alloc-stats")]
    let mut allocations = Vec::with_capacity(arguments.samples);
    #[cfg(feature = "alloc-stats")]
    let mut bytes_allocated = Vec::with_capacity(arguments.samples);
    #[cfg(feature = "alloc-stats")]
    let mut net_bytes = Vec::with_capacity(arguments.samples);

    eprintln!("F5_STAGE measured_samples count={}", arguments.samples);
    for sample in 0..arguments.samples {
        #[cfg(feature = "alloc-stats")]
        let allocation_region = Region::new(GLOBAL);
        let started = Instant::now();
        for _ in 0..arguments.requests_per_sample {
            execute_request(&client, &url, &body)?;
        }
        let elapsed = started.elapsed();
        #[cfg(feature = "alloc-stats")]
        let allocation = allocation_region.change();

        wall_ms.push(elapsed.as_secs_f64() * 1000.0);
        requests_per_second.push(arguments.requests_per_sample as f64 / elapsed.as_secs_f64());
        #[cfg(feature = "alloc-stats")]
        {
            allocations.push(allocation.allocations as f64);
            bytes_allocated.push(allocation.bytes_allocated as f64);
            net_bytes.push(
                (allocation.bytes_allocated as i128 - allocation.bytes_deallocated as i128) as f64,
            );
        }
        #[cfg(feature = "alloc-stats")]
        eprintln!(
            "F5_SAMPLE {} wall_ms={:.6} requests_per_second={:.6} allocations={} bytes_allocated={} net_bytes={}",
            sample + 1,
            wall_ms[sample],
            requests_per_second[sample],
            allocation.allocations,
            allocation.bytes_allocated,
            allocation.bytes_allocated as i128 - allocation.bytes_deallocated as i128
        );
        #[cfg(not(feature = "alloc-stats"))]
        eprintln!(
            "F5_SAMPLE {} wall_ms={:.6} requests_per_second={:.6}",
            sample + 1,
            wall_ms[sample],
            requests_per_second[sample]
        );
    }

    thread::sleep(Duration::from_millis(25));
    let metrics = engine.metrics();
    let current = metrics.current();
    let high = metrics.high_water();
    let expected_requests = arguments.warmups + arguments.samples * arguments.requests_per_sample;
    let operation_gauges_zero = current.inflight_requests() == 0
        && current.inflight_resolutions() == 0
        && current.standalone_tcp_connections() == 0;
    let queue_gauges_zero = current.queued_commands() == 0
        && current.queued_callbacks() == 0
        && current.reserved_stream_queue_bytes() == 0
        && current.reserved_tcp_queue_bytes() == 0
        && current.connection_waiters() == 0;
    let keepalive_state_exact =
        current.active_connections() == 1 && current.idle_connections() == 1;
    let connection_reuse_exact = metrics.connection_metrics_available()
        && metrics.requests_accepted() == expected_requests as u64
        && metrics.requests_completed() == expected_requests as u64
        && metrics.requests_failed() == 0
        && metrics.requests_cancelled() == 0
        && metrics.connections_opened() == 1
        && metrics.connections_reused() == (expected_requests - 1) as u64;

    eprintln!("F5_STAGE shutdown");
    engine.shutdown()?;
    let (server_connections, server_requests) = server.stop()?;
    let bytes_exact = server_requests == expected_requests;
    let fixture_gone = true;
    let joined_shutdown = true;
    let connection_reuse_exact = connection_reuse_exact && server_connections == 1;
    let checks = Checks {
        bytes_exact,
        connection_reuse_exact,
        operation_gauges_zero,
        queue_gauges_zero,
        keepalive_state_exact,
        joined_shutdown,
        fixture_gone,
    };
    if !checks.bytes_exact
        || !checks.connection_reuse_exact
        || !checks.operation_gauges_zero
        || !checks.queue_gauges_zero
        || !checks.keepalive_state_exact
        || !checks.joined_shutdown
        || !checks.fixture_gone
    {
        return Err("one or more correctness/quiescence checks failed".into());
    }

    let mut measures = Vec::new();
    measures.push(("wall_ms", Summary::measured("ms", &wall_ms)?));
    measures.push((
        "requests_per_second",
        Summary::measured("requests/s", &requests_per_second)?,
    ));
    #[cfg(feature = "alloc-stats")]
    {
        measures.push((
            "allocations_per_sample",
            Summary::measured("allocations", &allocations)?,
        ));
        measures.push((
            "bytes_allocated_per_sample",
            Summary::measured("bytes", &bytes_allocated)?,
        ));
        measures.push((
            "net_bytes_per_sample",
            Summary::measured("bytes", &net_bytes)?,
        ));
    }
    #[cfg(not(feature = "alloc-stats"))]
    {
        measures.push((
            "allocations_per_sample",
            Summary::unavailable("allocations"),
        ));
        measures.push(("bytes_allocated_per_sample", Summary::unavailable("bytes")));
        measures.push(("net_bytes_per_sample", Summary::unavailable("bytes")));
    }
    measures.push((
        "connections_opened",
        Summary::measured("connections", &[metrics.connections_opened() as f64])?,
    ));
    measures.push((
        "connections_reused",
        Summary::measured("connections", &[metrics.connections_reused() as f64])?,
    ));
    measures.push((
        "high_inflight_requests",
        Summary::measured("requests", &[high.inflight_requests() as f64])?,
    ));
    measures.push((
        "high_queued_commands",
        Summary::measured("commands", &[high.queued_commands() as f64])?,
    ));
    measures.push((
        "high_queued_callbacks",
        Summary::measured("callbacks", &[high.queued_callbacks() as f64])?,
    ));
    measures.push((
        "high_reserved_stream_queue_bytes",
        Summary::measured("bytes", &[high.reserved_stream_queue_bytes() as f64])?,
    ));
    measures.push((
        "high_inflight_resolutions",
        Summary::measured("resolutions", &[high.inflight_resolutions() as f64])?,
    ));
    measures.push((
        "high_standalone_tcp_connections",
        Summary::measured("connections", &[high.standalone_tcp_connections() as f64])?,
    ));
    measures.push((
        "high_reserved_tcp_queue_bytes",
        Summary::measured("bytes", &[high.reserved_tcp_queue_bytes() as f64])?,
    ));
    measures.push((
        "high_active_connections",
        Summary::measured("connections", &[high.active_connections() as f64])?,
    ));
    measures.push((
        "high_idle_connections",
        Summary::measured("connections", &[high.idle_connections() as f64])?,
    ));
    measures.push((
        "high_connection_waiters",
        Summary::measured("waiters", &[high.connection_waiters() as f64])?,
    ));
    measures.push(("process_wall_ms", Summary::unavailable("ms")));
    measures.push(("process_cpu_ms", Summary::unavailable("ms")));
    measures.push((
        "process_peak_working_set_bytes",
        Summary::unavailable("bytes"),
    ));
    measures.push((
        "process_sampled_peak_private_bytes",
        Summary::unavailable("bytes"),
    ));

    let mut output = String::new();
    write!(
        output,
        "{{\"schema\":{},\"source\":{{\"commit\":{},\"package_version\":{}}},\"platform\":{{\"target\":{},\"os\":{},\"arch\":{},\"rustc\":{},\"cargo_features\":{}}},\"workload\":{{\"id\":{},\"sample_count\":{},\"warmup_count\":{},\"requests_per_sample\":{},\"body_bytes\":{},\"allocator_instrumented\":{},\"fixture_scope\":\"in_process\",\"fixture_accept_poll_ms\":1}},\"process_observation\":null,\"measures\":{{",
        json_string(SCHEMA),
        json_string(&arguments.source_commit),
        json_string(&arguments.source_version),
        json_string(env!("NBREQ_F5_TARGET")),
        json_string(std::env::consts::OS),
        json_string(std::env::consts::ARCH),
        json_string(env!("NBREQ_F5_RUSTC")),
        json_string(feature_selection()),
        json_string(WORKLOAD),
        arguments.samples,
        arguments.warmups,
        arguments.requests_per_sample,
        arguments.body_bytes,
        cfg!(feature = "alloc-stats")
    )?;
    let mut first = true;
    for (name, summary) in &measures {
        push_metric(&mut output, &mut first, name, summary)?;
    }
    write!(
        output,
        "}},\"checks\":{{\"bytes_exact\":{},\"connection_reuse_exact\":{},\"operation_gauges_zero\":{},\"queue_gauges_zero\":{},\"keepalive_state_exact\":{},\"joined_shutdown\":{},\"fixture_gone\":{},\"no_leftover_process\":null}}}}",
        checks.bytes_exact,
        checks.connection_reuse_exact,
        checks.operation_gauges_zero,
        checks.queue_gauges_zero,
        checks.keepalive_state_exact,
        checks.joined_shutdown,
        checks.fixture_gone
    )?;
    println!("{output}");
    Ok(())
}
