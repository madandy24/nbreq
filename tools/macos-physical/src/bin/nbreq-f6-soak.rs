use std::error::Error;
use std::thread;
use std::time::{Duration, Instant};

use nbreq::{
    Engine, EngineConfig, Request, ResolveRequest, ResolveStatus, ResourceMetrics,
    TcpConnectRequest,
};
use nbreq_macos_physical::{env_duration, env_u16, event, urls_from_env};

fn main() -> Result<(), Box<dyn Error>> {
    let duration = env_duration("NBREQ_SOAK_SECONDS", 12 * 60 * 60)?;
    let interval = env_duration("NBREQ_SOAK_INTERVAL_SECONDS", 60)?;
    let urls = urls_from_env()?;
    let dns_name =
        std::env::var("NBREQ_SOAK_DNS_NAME").unwrap_or_else(|_| "example.com".to_owned());
    let tcp_host =
        std::env::var("NBREQ_SOAK_TCP_HOST").unwrap_or_else(|_| "example.com".to_owned());
    let tcp_port = env_u16("NBREQ_SOAK_TCP_PORT", 80)?;

    // Validate every caller-controlled spelling before starting the long-lived owner.
    for url in &urls {
        Request::get(url).build()?;
    }
    ResolveRequest::hostname(&dns_name).build()?;
    TcpConnectRequest::hostname(&tcp_host, tcp_port).build()?;

    let engine = Engine::new(EngineConfig::spawned())?;
    let client = engine.client();
    let started = Instant::now();
    let mut cycle = 0_u64;
    let mut http_ok = 0_u64;
    let mut dns_ok = 0_u64;
    let mut tcp_ok = 0_u64;
    let mut errors = 0_u64;

    event(format!(
        "SOAK_START pid={} duration_s={} interval_s={} urls={urls:?} dns={dns_name:?} tcp={tcp_host:?}:{tcp_port}",
        std::process::id(),
        duration.as_secs(),
        interval.as_secs()
    ));

    while started.elapsed() < duration {
        cycle = cycle.saturating_add(1);
        for url in &urls {
            let request = Request::get(url)
                .connect_timeout(Duration::from_secs(10))
                .total_timeout(Duration::from_secs(20))
                .build()?;
            match client.execute(request) {
                Ok(response) => {
                    http_ok = http_ok.saturating_add(1);
                    event(format!(
                        "HTTP_OK cycle={cycle} url={url:?} status={} body_bytes={}",
                        response.status(),
                        response.body().len()
                    ));
                }
                Err(error) => {
                    errors = errors.saturating_add(1);
                    event(format!(
                        "HTTP_ERROR cycle={cycle} url={url:?} error={error:?}"
                    ));
                }
            }
        }

        if cycle == 1 || cycle % 10 == 0 {
            match engine.resolver().execute(
                ResolveRequest::hostname(&dns_name)
                    .total_timeout(Duration::from_secs(15))
                    .build()?,
            ) {
                Ok(answer)
                    if answer.status() == ResolveStatus::Answer
                        && !answer.addresses().is_empty() =>
                {
                    dns_ok = dns_ok.saturating_add(1);
                    event(format!(
                        "DNS_OK cycle={cycle} addresses={} candidate={:?}",
                        answer.addresses().len(),
                        answer.candidate_name()
                    ));
                }
                Ok(answer) => {
                    errors = errors.saturating_add(1);
                    event(format!(
                        "DNS_ERROR cycle={cycle} status={:?} addresses={}",
                        answer.status(),
                        answer.addresses().len()
                    ));
                }
                Err(error) => {
                    errors = errors.saturating_add(1);
                    event(format!("DNS_ERROR cycle={cycle} error={error:?}"));
                }
            }

            match engine.tcp_connector().execute(
                TcpConnectRequest::hostname(&tcp_host, tcp_port)
                    .connect_timeout(Duration::from_secs(15))
                    .read_inactivity_timeout(Duration::from_secs(15))
                    .write_inactivity_timeout(Duration::from_secs(15))
                    .build()?,
            ) {
                Ok(connection) => {
                    tcp_ok = tcp_ok.saturating_add(1);
                    event(format!(
                        "TCP_OK cycle={cycle} peer={}",
                        connection.peer_addr()?
                    ));
                    drop(connection);
                }
                Err(error) => {
                    errors = errors.saturating_add(1);
                    event(format!("TCP_ERROR cycle={cycle} error={error:?}"));
                }
            }
        }

        let current = wait_for_quiescence(&engine);
        let metrics = engine.metrics();
        event(format!(
            "METRICS cycle={cycle} requests={} completed={} cancelled={} failed={} inflight={} queued_commands={} queued_callbacks={} stream_bytes={} resolves={} tcp={} tcp_bytes={} active_connections={} idle_connections={} connection_waiters={}",
            metrics.requests_accepted(),
            metrics.requests_completed(),
            metrics.requests_cancelled(),
            metrics.requests_failed(),
            current.inflight_requests(),
            current.queued_commands(),
            current.queued_callbacks(),
            current.reserved_stream_queue_bytes(),
            current.inflight_resolutions(),
            current.standalone_tcp_connections(),
            current.reserved_tcp_queue_bytes(),
            current.active_connections(),
            current.idle_connections(),
            current.connection_waiters()
        ));
        if !is_quiescent(current) {
            errors = errors.saturating_add(1);
            event(format!(
                "RESOURCE_NOT_QUIESCENT cycle={cycle} current={current:?}"
            ));
        }

        let remaining = duration.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(interval.min(remaining));
    }

    let final_metrics = engine.metrics();
    engine.shutdown()?;
    event(format!(
        "SOAK_END cycles={cycle} http_ok={http_ok} dns_ok={dns_ok} tcp_ok={tcp_ok} errors={errors} requests_accepted={} requests_failed={} resolutions_accepted={} resolutions_failed={} tcp_connects_accepted={} tcp_connects_failed={}",
        final_metrics.requests_accepted(),
        final_metrics.requests_failed(),
        final_metrics.resolutions_accepted(),
        final_metrics.resolutions_failed(),
        final_metrics.tcp_connects_accepted(),
        final_metrics.tcp_connects_failed()
    ));
    if errors != 0 {
        return Err(format!("soak completed with {errors} errors").into());
    }
    Ok(())
}

fn wait_for_quiescence(engine: &Engine) -> ResourceMetrics {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let current = engine.metrics().current();
        if is_quiescent(current) || Instant::now() >= deadline {
            return current;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn is_quiescent(current: ResourceMetrics) -> bool {
    current.inflight_requests() == 0
        && current.queued_commands() == 0
        && current.queued_callbacks() == 0
        && current.reserved_stream_queue_bytes() == 0
        && current.inflight_resolutions() == 0
        && current.standalone_tcp_connections() == 0
        && current.reserved_tcp_queue_bytes() == 0
        && current.connection_waiters() == 0
}
