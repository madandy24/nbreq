//! Drive HTTP from the host loop: `cargo run --example A08-http-manual -- [URL]`.
use std::time::{Duration, Instant};

use nbreq::{Completion, Engine, EngineBuilder, Request};

fn fetch(engine: &mut Engine, url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let pending = engine.client().submit(
        Request::get(url)
            .total_timeout(Duration::from_secs(15))
            .build()?,
    )?;
    while !pending.is_complete() {
        // Process application events here. Each drive pass may wait up to this deadline.
        engine.drive(Instant::now() + Duration::from_millis(10))?;
    }
    // This also provides a simpler alternative when no host work needs interleaving.
    match engine.drive_until(pending)? {
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
    let mut engine = EngineBuilder::manual().build()?;
    let result = fetch(&mut engine, &url);
    engine.shutdown()?;
    result
}
