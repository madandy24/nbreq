use std::num::NonZeroUsize;
use std::time::{Duration, Instant};
use std::{fs, thread};

use nbreq::{
    Client, Completion, Engine, EngineConfig, EngineMetrics, Request, Response, ResponseReader,
    StreamRead, StreamRequest,
};
use serde_json::{Value, json};

use crate::{Args, Result, acknowledge, argument, emit};

#[derive(Default)]
struct Outcome {
    accepted: usize,
    completed: usize,
    cancelled: usize,
    response_bytes: usize,
    retained_bytes: usize,
    latencies_ms: Vec<f64>,
}

fn metrics_json(metrics: EngineMetrics) -> Value {
    let current = metrics.current();
    let high = metrics.high_water();
    json!({"accepted":metrics.requests_accepted(), "completed":metrics.requests_completed(),
        "failed":metrics.requests_failed(), "cancelled":metrics.requests_cancelled(),
        "opened":metrics.connections_opened(), "reused":metrics.connections_reused(),
        "closed":metrics.connections_closed(), "inflight":current.inflight_requests(),
        "active_connections":current.active_connections(), "idle_connections":current.idle_connections(),
        "stream_reserved_bytes":current.reserved_stream_queue_bytes(),
        "buffered_reserved_bytes":current.reserved_buffered_body_bytes(),
        "high_buffered_reserved_bytes":high.reserved_buffered_body_bytes(),
        "high_inflight":high.inflight_requests(), "high_active_connections":high.active_connections(),
        "high_stream_reserved_bytes":high.reserved_stream_queue_bytes(),
        "queued_commands":current.queued_commands(), "queued_callbacks":current.queued_callbacks(),
        "connection_waiters":current.connection_waiters()})
}

fn quiescent(engine: &Engine) -> bool {
    let state = engine.metrics().current();
    state.inflight_requests() == 0
        && state.queued_commands() == 0
        && state.queued_callbacks() == 0
        && state.connection_waiters() == 0
        && state.reserved_stream_queue_bytes() == 0
        && state.reserved_tcp_queue_bytes() == 0
        && state.inflight_resolutions() == 0
        && state.standalone_tcp_connections() == 0
        && state.active_connections() == state.idle_connections()
}

fn settle(engine: &Engine) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !quiescent(engine) {
        if Instant::now() >= deadline {
            return Err(format!(
                "operation/queue gauges failed to settle: {}",
                metrics_json(engine.metrics())
            )
            .into());
        }
        thread::sleep(Duration::from_millis(1));
    }
    Ok(())
}

#[cfg(feature = "alloc-meter")]
fn allocation_json(before: nbreq_f5_meter::Snapshot, after: nbreq_f5_meter::Snapshot) -> Value {
    json!({"live_before":before.live_bytes, "live_after":after.live_bytes, "peak_live":after.peak_live_bytes,
        "peak_above_start":after.peak_live_bytes.saturating_sub(before.live_bytes),
        "allocations":after.allocations - before.allocations, "deallocations":after.deallocations - before.deallocations,
        "reallocations":after.reallocations - before.reallocations,
        "bytes_requested":after.bytes_requested - before.bytes_requested,
        "bytes_released":after.bytes_released - before.bytes_released})
}

fn phase(
    name: &str,
    engine: Option<&Engine>,
    require_quiet: bool,
    work: impl FnOnce() -> Result<Outcome>,
) -> Result<()> {
    emit(json!({"event":"phase_begin", "phase":name}))?;
    acknowledge()?;
    let before_metrics = engine.map(Engine::metrics);
    #[cfg(feature = "alloc-meter")]
    let before_alloc = crate::ALLOCATOR.begin_phase();
    let start = Instant::now();
    let mut result = work()?;
    if require_quiet {
        if let Some(engine) = engine {
            settle(engine)?;
        }
    }
    let wall_ms = start.elapsed().as_secs_f64() * 1000.0;
    let after_metrics = engine.map(Engine::metrics);
    #[cfg(feature = "alloc-meter")]
    let after_alloc = crate::ALLOCATOR.snapshot();
    if let (Some(before), Some(after)) = (before_metrics, after_metrics) {
        if after.requests_failed() != before.requests_failed() {
            return Err("unexpected failed request invalidates measurement".into());
        }
        if after.requests_accepted() - before.requests_accepted() != result.accepted as u64 {
            return Err("accepted request accounting mismatch".into());
        }
        if require_quiet
            && (after.requests_completed() - before.requests_completed() != result.completed as u64
                || after.requests_cancelled() - before.requests_cancelled()
                    != result.cancelled as u64)
        {
            return Err(format!("terminal accounting mismatch in {name}").into());
        }
    }
    result.latencies_ms.sort_by(f64::total_cmp);
    let percentile = |fraction: f64| -> Option<f64> {
        if result.latencies_ms.is_empty() {
            None
        } else {
            Some(
                result.latencies_ms
                    [((result.latencies_ms.len() - 1) as f64 * fraction).ceil() as usize],
            )
        }
    };
    #[cfg(feature = "alloc-meter")]
    let allocation = allocation_json(before_alloc, after_alloc);
    #[cfg(not(feature = "alloc-meter"))]
    let allocation = Value::Null;
    emit(
        json!({"event":"phase_end", "phase":name, "wall_ms":wall_ms, "accepted":result.accepted,
        "completed":result.completed, "cancelled":result.cancelled, "response_bytes":result.response_bytes,
        "retained_response_bytes":result.retained_bytes, "latency_p50_ms":percentile(0.5), "latency_p95_ms":percentile(0.95),
        "requests_per_second":if result.completed == 0 { None } else { Some(result.completed as f64 * 1000.0 / wall_ms) },
        "allocation":allocation, "metrics":after_metrics.map(metrics_json), "quiescent":require_quiet}),
    )?;
    acknowledge()?;
    Ok(())
}

fn body(size: usize) -> Vec<u8> {
    (0..size).map(|index| (index % 251) as u8).collect()
}

fn check_body(bytes: &[u8], offset: usize) -> Result<()> {
    if bytes
        .iter()
        .enumerate()
        .any(|(index, byte)| *byte != ((offset + index) % 251) as u8)
    {
        return Err("response bytes differ from the fixture".into());
    }
    Ok(())
}

fn submit(
    client: &Client,
    base: &str,
    size: usize,
    hold_ms: u64,
    chunk_ms: u64,
) -> Result<nbreq::PendingRequest> {
    Ok(client.submit(
        Request::post(format!("{base}/body/{size}/{hold_ms}/{chunk_ms}"))
            .body(body(size))
            .total_timeout(Duration::from_secs(15))
            .build()?,
    )?)
}

#[allow(clippy::too_many_arguments)] // Keep workload parameters explicit at each measurement phase.
fn batch(
    client: &Client,
    base: &str,
    count: usize,
    size: usize,
    hold_ms: u64,
    chunk_ms: u64,
    retained: &mut Vec<Response>,
    outcome: &mut Outcome,
) -> Result<()> {
    let mut pending = Vec::with_capacity(count);
    for _ in 0..count {
        pending.push((
            Instant::now(),
            submit(client, base, size, hold_ms, chunk_ms)?,
        ));
    }
    outcome.accepted += count;
    for (start, pending) in pending {
        match pending.wait() {
            Completion::Completed(response) => {
                if response.status() != 200 || response.body().len() != size {
                    return Err("incorrect response status or length".into());
                }
                check_body(response.body(), 0)?;
                outcome
                    .latencies_ms
                    .push(start.elapsed().as_secs_f64() * 1000.0);
                outcome.completed += 1;
                outcome.response_bytes += size;
                retained.push(response);
            }
            other => return Err(format!("request did not complete successfully: {other:?}").into()),
        }
    }
    Ok(())
}

fn config(args: &Args) -> Result<EngineConfig> {
    let mut config = EngineConfig::spawned()
        .with_max_connections_per_origin(NonZeroUsize::new(32).ok_or("nonzero")?)
        .with_max_idle_connections_per_origin(32);
    if let Some(path) = args.get("--root") {
        config = config.with_additional_tls_root_certificate(fs::read(path)?);
    }
    if let Some(bytes) = args.get("--buffered-budget") {
        config = config.with_max_buffered_body_bytes(bytes.parse()?);
    }
    Ok(config)
}

pub fn verified(args: &Args) -> Result<()> {
    let engine = Engine::new(config(args)?)?;
    let start = Instant::now();
    let response = engine
        .get("https://example.com/")
        .total_timeout(Duration::from_secs(20))
        .call()?;
    if response.status() != 200 || response.body().is_empty() {
        return Err("public trusted HTTPS failed".into());
    }
    emit(
        json!({"event":"verified_https", "status":response.status(), "body_bytes":response.body().len(),
        "wall_ms":start.elapsed().as_secs_f64()*1000.0, "additional_root":args.contains_key("--root")}),
    )?;
    engine.shutdown()?;
    Ok(())
}

pub fn run(args: &Args) -> Result<()> {
    let base = argument(args, "--base", "");
    if !(base.starts_with("http://127.0.0.1:") || base.starts_with("https://127.0.0.1:")) {
        return Err("loopback fixture base is required".into());
    }
    let connections: usize = argument(args, "--connections", "1").parse()?;
    let size: usize = argument(args, "--body-bytes", "1024").parse()?;
    let rounds: usize = argument(args, "--rounds", "16").parse()?;
    let extended = argument(args, "--extended", "no") == "yes";
    if !(1..=32).contains(&connections)
        || !(1..=50 * 1024).contains(&size)
        || !(1..=256).contains(&rounds)
    {
        return Err("workload argument outside measurement bounds".into());
    }
    if base.starts_with("https") && !args.contains_key("--root") {
        return Err("verified HTTPS fixture requires its CA".into());
    }
    let settings = config(args)?;
    emit(
        json!({"event":"client_ready", "schema":"nbreq-f5-memory-v1", "pid":std::process::id(),
        "source":argument(args,"--source","unknown"), "target":env!("NBREQ_F5_TARGET"), "rustc":env!("NBREQ_F5_RUSTC"),
        "instrumented":cfg!(feature="alloc-meter"), "resolver":cfg!(feature="resolver"),
        "connections":connections, "body_bytes":size, "rounds":rounds, "extended":extended,
        "tls_verification":if base.starts_with("https:") { "platform_plus_fixture_CA" } else { "not_applicable" }, "fixture_scope":"separate_process",
        "config":{"inflight_requests":settings.max_inflight_requests().get(), "max_connections":settings.max_connections().get(),
            "per_origin_connections":settings.max_connections_per_origin().get(), "idle_per_origin":settings.max_idle_connections_per_origin(),
            "stream_window":settings.max_stream_queue_bytes_per_request(), "stream_budget":settings.max_stream_queued_bytes()}}),
    )?;
    phase("driver_idle", None, true, || {
        thread::sleep(Duration::from_millis(150));
        Ok(Outcome::default())
    })?;
    let mut engine = None;
    phase("engine_idle", None, true, || {
        engine = Some(Engine::new(settings)?);
        thread::sleep(Duration::from_millis(100));
        Ok(Outcome::default())
    })?;
    let engine = engine.ok_or("Engine was not constructed")?;
    let client = engine.client();
    let mut retained = Vec::with_capacity(connections);
    phase("cold_connections", Some(&engine), true, || {
        let mut outcome = Outcome::default();
        batch(
            &client,
            base,
            connections,
            size,
            250,
            0,
            &mut retained,
            &mut outcome,
        )?;
        retained.clear();
        Ok(outcome)
    })?;
    if engine.metrics().connections_opened() != connections as u64 {
        return Err("cold phase did not open the requested connection count".into());
    }
    let opened = engine.metrics().connections_opened();
    phase("steady", Some(&engine), true, || {
        let mut outcome = Outcome {
            latencies_ms: Vec::with_capacity(connections * rounds),
            ..Outcome::default()
        };
        for _ in 0..rounds {
            batch(
                &client,
                base,
                connections,
                size,
                0,
                0,
                &mut retained,
                &mut outcome,
            )?;
            retained.clear();
        }
        Ok(outcome)
    })?;
    if engine.metrics().connections_opened() != opened {
        return Err("steady phase failed exact connection reuse".into());
    }
    phase("burst_retained", Some(&engine), true, || {
        let mut outcome = Outcome::default();
        batch(
            &client,
            base,
            connections,
            size,
            100,
            0,
            &mut retained,
            &mut outcome,
        )?;
        thread::sleep(Duration::from_millis(150));
        outcome.retained_bytes = connections * size;
        Ok(outcome)
    })?;
    phase("released_idle", Some(&engine), true, || {
        retained.clear();
        thread::sleep(Duration::from_millis(300));
        Ok(Outcome::default())
    })?;
    if extended {
        extended_phases(&engine, &client, base, connections, &mut retained)?;
    }
    settle(&engine)?;
    let final_metrics = engine.metrics();
    if final_metrics.requests_accepted()
        != final_metrics.requests_completed() + final_metrics.requests_cancelled()
    {
        return Err("final terminal accounting mismatch".into());
    }
    if !extended
        && (final_metrics.connections_opened() != connections as u64
            || final_metrics.connections_reused()
                != final_metrics.requests_accepted() - connections as u64)
    {
        return Err("final reuse accounting mismatch".into());
    }
    drop(retained);
    drop(client);
    phase("shutdown_idle", None, true, || {
        engine.shutdown()?;
        thread::sleep(Duration::from_millis(150));
        Ok(Outcome::default())
    })?;
    emit(
        json!({"event":"client_done", "checks":{"exact_bytes":true,"exact_accounting":true,
        "quiescent":true,"joined_shutdown":true,"verified_tls":base.starts_with("https:")}, "metrics":metrics_json(final_metrics)}),
    )?;
    Ok(())
}

fn extended_phases(
    engine: &Engine,
    client: &Client,
    base: &str,
    connections: usize,
    retained: &mut Vec<Response>,
) -> Result<()> {
    phase("slow_peer", Some(engine), true, || {
        let mut outcome = Outcome::default();
        batch(
            client,
            base,
            connections,
            50 * 1024,
            0,
            2,
            retained,
            &mut outcome,
        )?;
        retained.clear();
        Ok(outcome)
    })?;
    phase("long_polls_and_burst", Some(engine), true, || {
        let polls = connections / 2;
        let mut waiting = Vec::with_capacity(polls);
        for _ in 0..polls {
            waiting.push(submit(client, base, 1024, 300, 0)?);
        }
        let mut outcome = Outcome::default();
        batch(
            client,
            base,
            connections - polls,
            8 * 1024,
            0,
            0,
            retained,
            &mut outcome,
        )?;
        if polls > 0 && waiting.iter().all(nbreq::PendingRequest::is_complete) {
            return Err(
                "long-poll case did not observe fast requests completing while polls waited".into(),
            );
        }
        retained.clear();
        for pending in waiting {
            match pending.wait() {
                Completion::Completed(response) => {
                    check_body(response.body(), 0)?;
                    if response.body().len() != 1024 {
                        return Err("poll response size".into());
                    }
                }
                other => return Err(format!("long poll failed: {other:?}").into()),
            }
        }
        outcome.accepted += polls;
        outcome.completed += polls;
        outcome.response_bytes += polls * 1024;
        Ok(outcome)
    })?;
    phase("cancel_and_replace", Some(engine), true, || {
        let mut waiting = Vec::with_capacity(connections);
        for _ in 0..connections {
            waiting.push(submit(client, base, 50 * 1024, 600, 0)?);
        }
        thread::sleep(Duration::from_millis(100));
        for pending in &waiting {
            pending.handle().cancel()?;
        }
        for pending in waiting {
            if !matches!(pending.wait(), Completion::Cancelled) {
                return Err("cancelled request delivered another terminal outcome".into());
            }
        }
        let mut outcome = Outcome {
            accepted: connections,
            cancelled: connections,
            ..Outcome::default()
        };
        batch(
            client,
            base,
            connections,
            1024,
            50,
            0,
            retained,
            &mut outcome,
        )?;
        retained.clear();
        Ok(outcome)
    })?;
    let stream_bytes = 512 * 1024;
    let mut readers: Vec<ResponseReader> = Vec::with_capacity(connections);
    phase("slow_reader_held", Some(engine), false, || {
        for _ in 0..connections {
            readers.push(
                client.submit_stream(
                    StreamRequest::get(format!("{base}/body/{stream_bytes}/0/0"))
                        .total_timeout(Duration::from_secs(15))
                        .build()?,
                )?,
            );
        }
        for reader in &mut readers {
            if reader.wait_head()?.status() != 200 {
                return Err("stream response status".into());
            }
        }
        thread::sleep(Duration::from_millis(300));
        if engine.metrics().current().inflight_requests() == 0
            || engine.metrics().current().reserved_stream_queue_bytes() == 0
        {
            let mut diagnostic = [0; 1];
            return Err(format!(
                "held streaming readers did not exercise backpressure: {}; read={:?}",
                metrics_json(engine.metrics()),
                readers[0].try_read(&mut diagnostic)
            )
            .into());
        }
        Ok(Outcome {
            accepted: connections,
            ..Outcome::default()
        })
    })?;
    let completed_before = engine.metrics().requests_completed();
    phase("slow_reader_drain", Some(engine), false, || {
        let mut bytes = [0; 1024];
        let mut offsets = vec![0; connections];
        let mut ended = vec![false; connections];
        while ended.iter().any(|done| !done) {
            for (index, reader) in readers.iter_mut().enumerate() {
                if ended[index] {
                    continue;
                }
                match reader.try_read(&mut bytes)? {
                    StreamRead::Data(read) => {
                        check_body(&bytes[..read], offsets[index])?;
                        offsets[index] += read;
                    }
                    StreamRead::Eof => {
                        if offsets[index] != stream_bytes {
                            return Err("stream bytes truncated".into());
                        }
                        ended[index] = true;
                    }
                    StreamRead::Pending => {}
                    _ => return Err("unknown stream result".into()),
                }
            }
            thread::sleep(Duration::from_millis(1));
        }
        readers.clear();
        settle(engine)?;
        Ok(Outcome {
            completed: (engine.metrics().requests_completed() - completed_before) as usize,
            response_bytes: connections * stream_bytes,
            ..Outcome::default()
        })
    })?;
    phase("large_transfer_retained", Some(engine), true, || {
        let mut outcome = Outcome::default();
        batch(
            client,
            base,
            1,
            4 * 1024 * 1024,
            0,
            0,
            retained,
            &mut outcome,
        )?;
        thread::sleep(Duration::from_millis(100));
        outcome.retained_bytes = 4 * 1024 * 1024;
        Ok(outcome)
    })?;
    phase("post_large_idle", Some(engine), true, || {
        retained.clear();
        thread::sleep(Duration::from_millis(500));
        Ok(Outcome::default())
    })?;
    Ok(())
}
