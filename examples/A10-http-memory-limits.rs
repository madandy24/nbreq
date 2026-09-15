//! Budget and body ownership: `cargo run --example A10-http-memory-limits -- [URL]`.
use std::num::NonZeroUsize;
use std::time::Duration;

use nbreq::{Engine, ErrorKind, ExecuteError, LimitKind};

fn fetch(engine: &Engine, url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let result = engine
        .get(url)
        .max_response_body_bytes(1024 * 1024)
        .total_timeout(Duration::from_secs(15))
        .call();
    if let Err(ExecuteError::Submission(error) | ExecuteError::Failed(error)) = &result {
        if error.kind() == ErrorKind::Limit {
            match error.limit_kind() {
                Some(LimitKind::BufferedBodyBytes) => {
                    eprintln!("buffer budget exhausted; release retained bodies or admit less work")
                }
                limit => eprintln!("request exceeded limit: {limit:?}"),
            }
        }
    }
    let response = result?;
    println!(
        "HTTP {}, {} bytes",
        response.status(),
        response.body().len()
    );
    println!(
        "retained/reserved body capacity: {} bytes",
        engine.metrics().current().reserved_buffered_body_bytes()
    );
    // Borrow for inspection; cloning a Response shares its immutable body allocation.
    println!("{}", String::from_utf8_lossy(response.body()));
    let bytes = response
        .into_body()
        .try_into_vec()
        .unwrap_or_else(|shared| shared.as_bytes().to_vec());
    // Taking the Vec transfers accounting/ownership to us; it does not free process memory.
    println!("application now owns {} bytes", bytes.len());
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://httpbin.org/get".into());
    // Illustrative for small workloads. This is not a whole-process RAM cap.
    let engine = Engine::builder()
        .max_connections(NonZeroUsize::new(4).ok_or("invalid connection count")?)
        .max_inflight_requests(NonZeroUsize::new(8).ok_or("invalid request count")?)
        .max_buffered_body_bytes(8 * 1024 * 1024)
        .build()?;
    let result = fetch(&engine, &url);
    engine.shutdown()?;
    result
}
