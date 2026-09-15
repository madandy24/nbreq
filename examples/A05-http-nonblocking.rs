//! Submit without waiting: `cargo run --example A05-http-nonblocking -- [URL]`.
use std::time::Duration;

use nbreq::{Completion, Engine, Request};

fn fetch(engine: &Engine, url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let client = engine.client();
    let mut pending = Vec::new();
    for _ in 0..2 {
        pending.push(
            client.submit(
                Request::get(url)
                    .total_timeout(Duration::from_secs(15))
                    .build()?,
            )?,
        );
    }
    // Both requests can progress while the application does other work.
    println!("submitted two requests; application is free to do other work");
    for request in pending {
        println!("ready now: {}", request.is_complete());
        // Wait only when we need the result. The request's total deadline still applies.
        match request.wait() {
            Completion::Completed(response) => println!("HTTP {}", response.status()),
            Completion::Failed(error) => return Err(error.into()),
            Completion::Cancelled => return Err("request cancelled".into()),
            _ => return Err("unrecognized completion".into()),
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://httpbin.org/get".into());
    let engine = Engine::builder().build()?;
    let result = fetch(&engine, &url);
    engine.shutdown()?;
    result
}
