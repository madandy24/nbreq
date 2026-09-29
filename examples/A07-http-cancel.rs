//! Cancel a demonstrably outstanding call: `cargo run --example A07-http-cancel`.
//! Uses a local server that acknowledges the request and deliberately withholds its response.
#[path = "support/held_http.rs"]
mod held_http;

use std::time::Duration;

use nbreq::{Completion, Engine, Request, WaitOutcome};

fn cancel_request(
    engine: &Engine,
    server: &held_http::Server,
) -> Result<(), Box<dyn std::error::Error>> {
    let request = Request::get(server.url())
        // .connect_timeout(std::time::Duration::from_secs(10)) // Default setting.
        // .inactivity_timeout(std::time::Duration::from_secs(30)) // Default setting.
        // .total_timeout(std::time::Duration::from_secs(120)) // Default setting.
        .build()?;
    let pending = engine.client().submit(request)?;
    server.wait_for_request()?;
    // The server has received this request and cannot complete it before we release it.
    assert!(
        !pending.is_complete(),
        "the request must still be outstanding"
    );
    println!("server received the request; cancelling it now");
    pending.handle().cancel()?;
    // cancel() requests cancellation. The terminal completion proves who won the race.
    match pending.wait_for(Duration::from_secs(5)) {
        WaitOutcome::Completed(Completion::Cancelled) => {
            println!("completion: Cancelled (verified)")
        }
        other => return Err(format!("expected Cancelled, got {other:?}").into()),
    }
    // For all outstanding work on this Engine, use engine.cancel_all(). It keeps the Engine alive.
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let server = held_http::Server::start()?;
    let engine = Engine::builder().build()?;
    let result = cancel_request(&engine, &server);
    engine.shutdown()?;
    server.stop()?;
    result
}
