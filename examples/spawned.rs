//! Run with `cargo run --example spawned -- http://example.com/`.
use std::sync::mpsc;
use std::time::Duration;

use nbreq::{Completion, Engine, EngineConfig, Request};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args().nth(1).ok_or("usage: spawned URL")?;
    let engine = Engine::new(EngineConfig::spawned())?;
    let client = engine.client();
    let (sender, receiver) = mpsc::sync_channel(1);
    let request = Request::get(url)
        .total_timeout(Duration::from_secs(15))
        .build()?;
    let handle = client.start(request, move |completion| {
        // Send owned results to the application; keep the callback short.
        let _ = sender.send(completion);
    })?;
    let result = receiver.recv_timeout(Duration::from_secs(20));
    if result.is_err() {
        handle.cancel()?;
    }
    engine.shutdown()?;
    match result? {
        Completion::Completed(response) => println!("HTTP {}", response.status()),
        Completion::Failed(error) => return Err(error.into()),
        Completion::Cancelled => return Err("request cancelled".into()),
        _ => return Err("unrecognized completion".into()),
    }
    Ok(())
}
