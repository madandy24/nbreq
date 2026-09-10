//! Run with `cargo run --example manual -- http://example.com/`.
use std::time::Duration;

use nbreq::{Completion, EngineBuilder, Request};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args().nth(1).ok_or("usage: manual URL")?;
    let mut engine = EngineBuilder::manual().build()?;
    let client = engine.client();
    let request = Request::get(url)
        .total_timeout(Duration::from_secs(15))
        .build()?;

    let pending = client.submit(request)?;
    let result = engine.drive_until(pending);
    engine.shutdown()?;
    match result? {
        Completion::Completed(response) => println!("HTTP {}", response.status()),
        Completion::Failed(error) => return Err(error.into()),
        Completion::Cancelled => return Err("request cancelled".into()),
        _ => return Err("unrecognized completion".into()),
    }
    Ok(())
}
