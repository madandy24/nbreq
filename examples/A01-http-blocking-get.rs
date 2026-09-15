//! Smallest GET: `cargo run --example A01-http-blocking-get -- [URL]`.
use std::time::Duration;

use nbreq::Engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://httpbin.org/get".into());
    let engine = Engine::builder().build()?;
    let result = engine
        .get(url)
        .total_timeout(Duration::from_secs(15))
        .call();
    engine.shutdown()?;
    let response = result?;
    println!("HTTP {}", response.status());
    println!("{}", String::from_utf8_lossy(response.body()));
    Ok(())
}
