use std::error::Error;
use std::io::{self, Write};
use std::thread;
use std::time::{Duration, Instant};

use nbreq::{Client, Engine, EngineConfig, ExecuteError, Request, TransportStage};
use nbreq_macos_physical::{event, urls_from_env};

const LOSS_DEADLINE: Duration = Duration::from_secs(180);
const RECOVERY_DEADLINE: Duration = Duration::from_secs(600);

fn main() -> Result<(), Box<dyn Error>> {
    let urls = urls_from_env()?;
    for url in &urls {
        Request::get(url).build()?;
    }
    let engine = Engine::new(EngineConfig::spawned())?;
    let client = engine.client();
    event(format!(
        "ENGINE_READY_NO_REQUESTS pid={} urls={urls:?}",
        std::process::id()
    ));
    println!(
        "Using an independent provider console/recovery path, disable the active network path; then press Enter."
    );
    io::stdout().flush()?;
    let mut line = String::new();
    io::stdin().read_line(&mut line)?;
    event("OUTAGE_REQUEST_LOOP_STARTED");

    let loss_started = Instant::now();
    let mut cycle = 0_u64;
    loop {
        cycle = cycle.saturating_add(1);
        let mut saw_dns_failure = false;
        for url in &urls {
            match execute(&client, url) {
                Err(ExecuteError::Failed(error))
                    if error.transport_stage() == Some(TransportStage::Dns) =>
                {
                    event(format!(
                        "DNS_FAILURE cycle={cycle} url={url:?} kind={:?} timeout={:?}",
                        error.kind(),
                        error.timeout_kind()
                    ));
                    saw_dns_failure = true;
                }
                result => event(format!(
                    "WAITING_FOR_DNS_FAILURE cycle={cycle} url={url:?} result={result:?}"
                )),
            }
        }
        if saw_dns_failure {
            break;
        }
        if loss_started.elapsed() >= LOSS_DEADLINE {
            return Err("the original Engine did not produce a DNS-stage failure".into());
        }
        thread::sleep(Duration::from_millis(250));
    }

    event("DNS_FAILURE_CONFIRMED; restore networking; recovery polling continues automatically");
    let recovery_started = Instant::now();
    let (recovered_url, recovered_status) = loop {
        cycle = cycle.saturating_add(1);
        let mut recovered = None;
        for url in &urls {
            match execute(&client, url) {
                Ok(status) => {
                    event(format!(
                        "ORIGINAL_ENGINE_RECOVERY_OK cycle={cycle} url={url:?} status={status} elapsed_ms={}",
                        recovery_started.elapsed().as_millis()
                    ));
                    recovered = Some((url.as_str(), status));
                    break;
                }
                Err(error) => event(format!(
                    "WAITING_FOR_ORIGINAL_ENGINE_RECOVERY cycle={cycle} url={url:?} result={error:?}"
                )),
            }
        }
        if let Some(recovered) = recovered {
            break recovered;
        }
        if recovery_started.elapsed() >= RECOVERY_DEADLINE {
            return Err("the original Engine did not recover after networking returned".into());
        }
        thread::sleep(Duration::from_millis(500));
    };

    let fresh = Engine::new(EngineConfig::spawned())?;
    let fresh_started = Instant::now();
    let fresh_status = execute(&fresh.client(), recovered_url)?;
    event(format!(
        "FRESH_ENGINE_OK url={recovered_url:?} status={fresh_status} elapsed_ms={}",
        fresh_started.elapsed().as_millis()
    ));
    fresh.shutdown()?;
    engine.shutdown()?;
    event(format!(
        "F6_REACQUISITION_PASS original_status={recovered_status} fresh_status={fresh_status}"
    ));
    Ok(())
}

fn execute(client: &Client, url: &str) -> Result<u16, ExecuteError> {
    let request = Request::get(url)
        .connect_timeout(Duration::from_secs(4))
        .total_timeout(Duration::from_secs(5))
        .build()
        .expect("validated probe URL must still build");
    client.execute(request).map(|response| response.status())
}
