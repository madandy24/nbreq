//! Explicit requests and engine reuse: `cargo run --example A03-http-full-get -- [URL]`.
use std::time::Duration;

use nbreq::{Engine, Request};

fn fetch_twice(engine: &Engine, url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let client = engine.client(); // Cheap command handle; the Engine remains the owner.
    for _ in 0..2 {
        let request = Request::get(url)
            .header("Accept", "application/json")
            .connect_timeout(Duration::from_secs(5))
            .inactivity_timeout(Duration::from_secs(10))
            .total_timeout(Duration::from_secs(15))
            .build()?;
        let response = client.execute(request)?;
        // HTTP 4xx/5xx are responses too. Transport failures return Err.
        println!(
            "HTTP {}, {} bytes",
            response.status(),
            response.body().len()
        );
        println!("headers: {:?}", response.headers());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://httpbin.org/get".into());
    let engine = Engine::builder().build()?;
    let result = fetch_twice(&engine, &url);
    engine.shutdown()?;
    result
}
