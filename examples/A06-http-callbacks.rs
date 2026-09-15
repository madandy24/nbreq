//! Deliver a completion to the application: `cargo run --example A06-http-callbacks -- [URL]`.
use std::sync::mpsc;
use std::time::Duration;

use nbreq::{Completion, Engine, Request};

fn fetch(engine: &Engine, url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let handle = engine.client().start(
        Request::get(url)
            .total_timeout(Duration::from_secs(15))
            .build()?,
        move |completion| {
            // Callback workers belong to the Engine. Keep callbacks short; hand work back.
            let _ = sender.send(completion);
        },
    )?;
    println!("request started; awaiting the callback's message");
    let result = receiver.recv_timeout(Duration::from_secs(20));
    if result.is_err() {
        handle.cancel()?;
    }
    match result? {
        Completion::Completed(response) => println!("HTTP {}", response.status()),
        Completion::Failed(error) => return Err(error.into()),
        Completion::Cancelled => return Err("request cancelled".into()),
        _ => return Err("unrecognized completion".into()),
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://httpbin.org/get".into());
    let engine = Engine::builder().build()?;
    let result = fetch(&engine, &url);
    engine.shutdown()?; // Joins the callback workers, including on an error path.
    result
}
