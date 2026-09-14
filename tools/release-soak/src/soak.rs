use std::fs;
use std::num::NonZeroUsize;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

#[cfg(feature = "resolver")]
use nbreq::{AddressFamily, CacheMode, ResolveCompletion, ResolveRequest, ResolveStatus};
use nbreq::{
    Completion, Engine, EngineConfig, EngineMetrics, ErrorKind, ExecuteError, LimitKind, Request,
    Response, StreamError, StreamRequest, TcpConnectRequest, UploadBody,
};
use serde_json::{Value, json};

use crate::{Args, Result, argument, emit};

const WAIT: Duration = Duration::from_secs(15);
const BODY_CAP: usize = 4 * 1024 * 1024;

fn config(root: &[u8], manual: bool) -> EngineConfig {
    let initial = if manual {
        EngineConfig::manual()
    } else {
        EngineConfig::spawned()
    };
    initial
        .with_additional_tls_root_certificate(root.to_vec())
        .with_max_connections(NonZeroUsize::new(8).expect("eight"))
        .with_max_connections_per_origin(NonZeroUsize::new(4).expect("four"))
        .with_max_idle_connections(8)
        .with_max_idle_connections_per_origin(4)
        .with_max_buffered_body_bytes(BODY_CAP)
        .with_max_request_body_bytes(3 * 1024 * 1024)
        .with_max_response_body_bytes(3 * 1024 * 1024)
        .with_max_stream_queue_bytes_per_request(8192)
        .with_max_stream_queued_bytes(64 * 1024)
        .with_max_queued_bytes(256 * 1024)
        .with_max_standalone_tcp_connections(NonZeroUsize::new(4).expect("four"))
        .with_max_tcp_queue_bytes_per_connection(8192)
}

fn body(bytes: &[u8], offset: usize) -> Result<()> {
    if bytes
        .iter()
        .enumerate()
        .any(|(i, byte)| *byte != ((offset + i) % 251) as u8)
    {
        return Err(format!("incorrect body at stream offset {offset}").into());
    }
    Ok(())
}

fn response(response: Response, size: usize) -> Result<Response> {
    if response.status() != 200 || response.body().len() != size {
        return Err(format!(
            "HTTP status/length mismatch: {} / {} expected {size}",
            response.status(),
            response.body().len()
        )
        .into());
    }
    body(response.body(), 0)?;
    Ok(response)
}

fn request(base: &str, size: usize, hold_ms: u64) -> Result<Request> {
    Ok(Request::get(format!("{base}/body/{size}/{hold_ms}/0"))
        .total_timeout(WAIT)
        .build()?)
}

fn completed(value: Completion, size: usize) -> Result<Response> {
    match value {
        Completion::Completed(value) => response(value, size),
        other => Err(format!("unexpected HTTP terminal: {other:?}").into()),
    }
}

pub fn metrics(m: EngineMetrics) -> Value {
    let c = m.current();
    let h = m.high_water();
    json!({"accepted":m.requests_accepted(),"completed":m.requests_completed(),
        "cancelled":m.requests_cancelled(),"failed":m.requests_failed(),
        "opened":m.connections_opened(),"closed":m.connections_closed(),"reused":m.connections_reused(),
        "inflight":c.inflight_requests(),"commands":c.queued_commands(),"callbacks":c.queued_callbacks(),
        "body_bytes":c.reserved_buffered_body_bytes(),"stream_bytes":c.reserved_stream_queue_bytes(),
        "tcp_bytes":c.reserved_tcp_queue_bytes(),"tcp":c.standalone_tcp_connections(),
        "resolves":c.inflight_resolutions(),"active":c.active_connections(),"idle":c.idle_connections(),
        "waiters":c.connection_waiters(),"high_body_bytes":h.reserved_buffered_body_bytes(),
        "high_stream_bytes":h.reserved_stream_queue_bytes(),"high_tcp_bytes":h.reserved_tcp_queue_bytes(),
        "high_active":h.active_connections(),"high_waiters":h.connection_waiters(),
        "dns_accepted":m.resolutions_accepted(),"dns_failed":m.resolutions_failed(),
        "tcp_accepted":m.tcp_connects_accepted(),"tcp_failed":m.tcp_connects_failed()})
}

fn settle(engine: &Engine) -> Result<()> {
    let deadline = Instant::now() + WAIT;
    loop {
        let c = engine.metrics().current();
        if c.inflight_requests() == 0
            && c.queued_commands() == 0
            && c.queued_callbacks() == 0
            && c.reserved_buffered_body_bytes() == 0
            && c.reserved_stream_queue_bytes() == 0
            && c.reserved_tcp_queue_bytes() == 0
            && c.standalone_tcp_connections() == 0
            && c.inflight_resolutions() == 0
            && c.connection_waiters() == 0
            && c.active_connections() == c.idle_connections()
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("non-quiescent resources: {}", metrics(engine.metrics())).into());
        }
        thread::sleep(Duration::from_millis(2));
    }
}

fn pressure(engine: &Engine, base: &str) -> Result<()> {
    let client = engine.client();
    let retained = response(
        client.execute(request(base, 2 * 1024 * 1024, 0)?)?,
        2 * 1024 * 1024,
    )?;
    let alias = retained.clone();
    drop(retained);
    if engine.metrics().current().reserved_buffered_body_bytes() < 2 * 1024 * 1024 {
        return Err("response alias lost its budget charge".into());
    }
    let mut spare = Vec::with_capacity(3 * 1024 * 1024);
    spare.push(0);
    match client.execute(
        Request::post("http://127.0.0.1:9/not-admitted")
            .body(spare)
            .total_timeout(WAIT)
            .build()?,
    ) {
        Err(ExecuteError::Submission(error))
            if error.limit_kind() == Some(LimitKind::BufferedBodyBytes) => {}
        other => {
            return Err(format!("spare capacity should fail before admission: {other:?}").into());
        }
    }
    match client.execute(request(base, 3 * 1024 * 1024, 0)?) {
        Err(ExecuteError::Failed(error))
            if error.kind() == ErrorKind::Limit
                && error.limit_kind() == Some(LimitKind::BufferedBodyBytes) => {}
        other => {
            return Err(
                format!("known response length should exceed remaining budget: {other:?}").into(),
            );
        }
    }
    drop(response(client.execute(request(base, 1024, 0)?)?, 1024)?);
    let transferred = alias
        .into_body()
        .try_into_vec()
        .map_err(|_| "sole response alias should transfer")?;
    body(&transferred, 0)?;
    drop(transferred);
    Ok(())
}

fn stream_round(engine: &Engine, base: &str) -> Result<()> {
    let mut reader = engine.client().submit_stream(
        StreamRequest::get(format!("{base}/body/524288/0/0"))
            .total_timeout(WAIT)
            .build()?,
    )?;
    if reader.wait_head()?.status() != 200 {
        return Err("stream status".into());
    }
    thread::sleep(Duration::from_millis(10));
    let mut bytes = [0; 4096];
    let mut count = 0;
    while let Some(read) = reader.read(&mut bytes)? {
        body(&bytes[..read], count)?;
        count += read;
        thread::sleep(Duration::from_millis(1));
    }
    if count != 524288 {
        return Err("stream length".into());
    }
    drop(reader);
    let (upload, mut sender) = UploadBody::fixed(32768, 8192)?;
    let reader = engine.client().submit_stream(
        StreamRequest::post(format!("{base}/body/1024/0/0"))
            .body_stream(upload)
            .total_timeout(WAIT)
            .build()?,
    )?;
    for offset in (0..32768).step_by(8192) {
        sender.push((offset..offset + 8192).map(|i| (i % 251) as u8).collect())?;
    }
    sender.finish()?;
    drop(response(reader.collect()?, 1024)?);
    Ok(())
}

fn stream_cancel(engine: &Engine, base: &str) -> Result<()> {
    let mut reader = engine.client().submit_stream(
        StreamRequest::get(format!("{base}/body/524288/0/10"))
            .total_timeout(WAIT)
            .build()?,
    )?;
    if reader.wait_head()?.status() != 200 {
        return Err("cancel stream status".into());
    }
    reader.handle().cancel()?;
    let mut bytes = [0; 8192];
    let mut count = 0;
    loop {
        match reader.read(&mut bytes) {
            Ok(Some(read)) => {
                body(&bytes[..read], count)?;
                count += read;
            }
            Err(StreamError::Cancelled) => break,
            other => return Err(format!("stream cancellation terminal: {other:?}").into()),
        }
    }
    Ok(())
}

fn lifecycle(root: &[u8], plain: &str, tls: &str) -> Result<()> {
    for manual in [false, true] {
        let mut engine = Engine::new(config(root, manual))?;
        let pending =
            engine
                .client()
                .submit(request(if manual { plain } else { tls }, 1024, 0)?)?;
        let terminal = if manual {
            engine.drive_until(pending)?
        } else {
            pending.wait()
        };
        drop(completed(terminal, 1024)?);
        let queued = engine.client().submit(request(plain, 1024, 2000)?)?;
        engine.shutdown()?;
        match queued.wait() {
            Completion::Cancelled => {}
            other => {
                return Err(format!("shutdown must terminate pending request: {other:?}").into());
            }
        }
    }
    Ok(())
}

pub fn run(args: &Args) -> Result<()> {
    let plain = args.get("--plain").ok_or("plain URL")?;
    let tls = args.get("--tls").ok_or("TLS URL")?;
    let tcp = args.get("--tcp").ok_or("TCP address")?.parse()?;
    let root = fs::read(args.get("--root").ok_or("DER root file")?)?;
    let seconds: u64 = argument(args, "--seconds", "14400").parse()?;
    let interval: u64 = argument(args, "--interval-ms", "500").parse()?;
    if seconds == 0 || seconds > 86400 || interval > 10000 {
        return Err("invalid soak bounds".into());
    }
    let engine = Engine::new(config(&root, false))?;
    let client = engine.client();
    for base in [plain, tls] {
        drop(response(client.execute(request(base, 1024, 0)?)?, 1024)?);
    }
    let start = Instant::now();
    let mut cycle = 0_u64;
    let mut expected_failures = 0_u64;
    let mut expected_cancellations = 0_u64;
    let mut network_due = Instant::now();
    let mut public_rounds = 0_u64;
    emit(
        json!({"event":"soak_start","pid":std::process::id(),"source":env!("R4_SOURCE"),
        "target":env!("R4_TARGET"),"rustc":env!("R4_RUSTC"),"resolver":cfg!(feature="resolver"),
        "debug_assertions":cfg!(debug_assertions),
        "duration_s":seconds,"interval_ms":interval,"body_cap":BODY_CAP,"private_root":true}),
    )?;
    while start.elapsed() < Duration::from_secs(seconds) {
        cycle += 1;
        let round = Instant::now();
        emit(
            json!({"event":"cycle_begin","cycle":cycle,"elapsed_s":start.elapsed().as_secs_f64()}),
        )?;
        let network = Instant::now() >= network_due;
        #[cfg(feature = "resolver")]
        let dns = if network {
            Some(
                engine.resolver().submit(
                    ResolveRequest::hostname("example.com.")
                        .address_family(AddressFamily::Ipv4)
                        .cache_mode(CacheMode::Bypass)
                        .total_timeout(WAIT)
                        .build()?,
                )?,
            )
        } else {
            None
        };
        let mut tcp_sessions = Vec::new();
        for _ in 0..2 {
            let mut connection = engine.tcp_connector().execute(
                TcpConnectRequest::literal(tcp)
                    .connect_timeout(WAIT)
                    .read_inactivity_timeout(WAIT)
                    .write_inactivity_timeout(WAIT)
                    .send_queue_bytes(4096)
                    .receive_queue_bytes(4096)
                    .build()?,
            )?;
            connection.send((0..65536).map(|i| (i % 251) as u8).collect())?;
            connection.finish()?;
            tcp_sessions.push(connection);
        }
        let polls = [
            client.submit(request(plain, 1024, 2000)?)?,
            client.submit(request(tls, 1024, 2000)?)?,
        ];
        let mut requests = Vec::new();
        for index in 0..16 {
            let size = [1024, 8192, 50 * 1024, 64 * 1024][index % 4];
            requests.push((
                client.submit(request(if index % 2 == 0 { plain } else { tls }, size, 20)?)?,
                size,
            ));
        }
        for (pending, size) in requests {
            drop(completed(pending.wait(), size)?);
        }
        if polls.iter().any(|poll| poll.is_complete()) {
            return Err("small burst did not complete while long polls remained pending".into());
        }
        for poll in polls {
            poll.handle().cancel()?;
            if !matches!(poll.wait(), Completion::Cancelled) {
                return Err("long poll cancellation".into());
            }
            expected_cancellations += 1;
        }
        stream_round(&engine, tls)?;
        for mut connection in tcp_sessions {
            let mut bytes = [0; 8192];
            let mut count = 0;
            while let Some(read) = connection.read(&mut bytes)? {
                body(&bytes[..read], count)?;
                count += read;
            }
            if count != 65536 {
                return Err("TCP exact echo/half-close length".into());
            }
        }
        if cycle == 1 || cycle % 10 == 0 {
            pressure(&engine, plain)?;
            expected_failures += 1;
            stream_cancel(&engine, tls)?;
            expected_cancellations += 1;
            let (send, receive) = mpsc::channel();
            client.start(request(plain, 1024, 0)?, move |terminal| {
                let _ = send.send(terminal);
            })?;
            drop(completed(receive.recv_timeout(WAIT)?, 1024)?);
            lifecycle(&root, plain, tls)?;
        }
        #[cfg(feature = "resolver")]
        if let Some(dns) = dns {
            match dns.wait() {
                ResolveCompletion::Completed(answer)
                    if answer.status() == ResolveStatus::Answer
                        && !answer.addresses().is_empty() => {}
                other => return Err(format!("external public DNS check: {other:?}").into()),
            }
        }
        if network {
            let live = client.execute(
                Request::get("https://example.com/")
                    .total_timeout(WAIT)
                    .build()?,
            )?;
            if live.status() != 200 || live.body().is_empty() {
                return Err("external hostname HTTPS check".into());
            }
            drop(live);
            public_rounds += 1;
            network_due = Instant::now() + Duration::from_secs(60);
            emit(json!({"event":"public_network_pass","cycle":cycle,"rounds":public_rounds}))?;
        }
        settle(&engine)?;
        let m = engine.metrics();
        if m.requests_failed() != expected_failures
            || m.requests_cancelled() != expected_cancellations
            || m.requests_accepted()
                != m.requests_completed() + m.requests_failed() + m.requests_cancelled()
            || m.tcp_connects_failed() != 0
            || m.resolutions_failed() != 0
            || m.high_water().reserved_buffered_body_bytes() > BODY_CAP
            || m.high_water().active_connections() > 8
            || m.high_water().reserved_stream_queue_bytes() > 65536
            || m.high_water().reserved_tcp_queue_bytes() > 32768
        {
            return Err(format!(
                "terminal accounting or resource bound mismatch: {}",
                metrics(m)
            )
            .into());
        }
        emit(
            json!({"event":"cycle_end","cycle":cycle,"elapsed_s":start.elapsed().as_secs_f64(),
            "round_s":round.elapsed().as_secs_f64(),"metrics":metrics(m),"checks":{"exact_bytes":true,"quiescent":true,"bounds":true,"terminals":true}}),
        )?;
        thread::sleep(
            Duration::from_millis(interval)
                .min(Duration::from_secs(seconds).saturating_sub(start.elapsed())),
        );
    }
    let before = engine.metrics();
    if cycle == 0
        || public_rounds == 0
        || before.connections_reused() == 0
        || before.high_water().connection_waiters() == 0
    {
        return Err("soak did not exercise required workload coverage".into());
    }
    let mut reader = client.submit_stream(
        StreamRequest::get(format!("{tls}/body/524288/0/10"))
            .total_timeout(WAIT)
            .build()?,
    )?;
    reader.wait_head()?;
    engine.shutdown()?;
    let mut bytes = [0; 8192];
    let mut count = 0;
    loop {
        match reader.read(&mut bytes) {
            Ok(Some(read)) => {
                body(&bytes[..read], count)?;
                count += read;
            }
            Err(StreamError::Cancelled) => break,
            other => return Err(format!("shutdown of live stream: {other:?}").into()),
        }
    }
    emit(
        json!({"event":"soak_end","cycles":cycle,"elapsed_s":start.elapsed().as_secs_f64(),
        "public_rounds":public_rounds,"metrics_before_shutdown":metrics(before),
        "checks":{"exact_bytes":true,"quiescent":true,"bounds":true,"terminals":true,"joined_shutdown":true,"live_stream_stopped":true}}),
    )?;
    Ok(())
}
