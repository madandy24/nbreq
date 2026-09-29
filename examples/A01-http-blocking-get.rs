//! Smallest GET: `cargo run --example A01-http-blocking-get -- [URL]`.
use nbreq::Engine;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://httpbin.org/get".into());
    let engine = Engine::builder().build()?;
    let result = engine
        .get(url)
        // .connect_timeout(std::time::Duration::from_secs(10)) // Default setting.
        // .inactivity_timeout(std::time::Duration::from_secs(30)) // Default setting.
        // .total_timeout(std::time::Duration::from_secs(120)) // Default setting.
        .call();
    engine.shutdown()?;
    let response = result?;
    println!("HTTP {}", response.status());
    println!("{}", String::from_utf8_lossy(response.body()));
    Ok(())
}
