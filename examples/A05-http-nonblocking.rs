//! Submit without waiting: `cargo run --example A05-http-nonblocking -- [URL]`.

use nbreq::{Completion, Engine, Request};

fn fetch(engine: &Engine, url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let client = engine.client();
    let mut pending = Vec::new();
    for _ in 0..2 {
        let request = Request::get(url)
            // .connect_timeout(std::time::Duration::from_secs(10)) // Default setting.
            // .inactivity_timeout(std::time::Duration::from_secs(30)) // Default setting.
            // .total_timeout(std::time::Duration::from_secs(120)) // Default setting.
            .build()?;
        pending.push(client.submit(request)?);
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
