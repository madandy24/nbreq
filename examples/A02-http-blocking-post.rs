//! Smallest POST: `cargo run --example A02-http-blocking-post -- [URL]`.
use std::time::Duration;

use nbreq::Engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://httpbin.org/post".into());
    let engine = Engine::builder().build()?;
    let result = engine
        .post(url)
        .header("Content-Type", "text/plain")
        .total_timeout(Duration::from_secs(15))
        .send("hello from nbreq");
    engine.shutdown()?;
    let response = result?;
    println!("HTTP {}", response.status());
    println!("{}", String::from_utf8_lossy(response.body()));
    Ok(())
}
