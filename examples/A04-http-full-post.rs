//! An explicit JSON POST: `cargo run --example A04-http-full-post -- [URL]`.

use nbreq::{Engine, Request};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://httpbin.org/post".into());
    let request = Request::post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json")
        .body(br#"{"message":"hello from nbreq"}"#.to_vec())
        .max_request_body_bytes(1024)
        .max_response_body_bytes(64 * 1024)
        // .connect_timeout(std::time::Duration::from_secs(10)) // Default setting.
        // .inactivity_timeout(std::time::Duration::from_secs(30)) // Default setting.
        // .total_timeout(std::time::Duration::from_secs(120)) // Default setting.
        .build()?;
    let engine = Engine::builder().build()?;
    let result = engine.client().execute(request);
    engine.shutdown()?;
    let response = result?;
    println!("HTTP {}", response.status());
    println!("{}", String::from_utf8_lossy(response.body()));
    Ok(())
}
