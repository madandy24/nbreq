use std::time::Duration;

use nbreq::{Completion, Engine, EngineConfig, Request, ShutdownOutcome};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::new(EngineConfig::spawned())?;
    let client = engine.client();
    let second_client = client.clone();

    let callback_request = Request::get("https://example.invalid/").build()?;
    if let Ok(handle) = client.start(callback_request, move |completion| match completion {
        Completion::Completed(response) => {
            let _status = response.status();
        }
        Completion::Failed(_) | Completion::Cancelled => {}
        _ => {}
    }) {
        handle.cancel()?;
    }

    let blocking_request = Request::get("https://example.invalid/").build()?;
    let _blocking_result = second_client.execute(blocking_request);

    engine.cancel_all();
    match engine.shutdown_for(Duration::ZERO)? {
        ShutdownOutcome::Complete => {}
        ShutdownOutcome::CallbacksRemaining(callbacks) => callbacks.wait()?,
        _ => {}
    }
    Ok(())
}
