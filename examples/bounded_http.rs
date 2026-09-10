//! Run with `cargo run --example bounded_http -- https://example.com/`.
use std::num::NonZeroUsize;
use std::time::Duration;

use nbreq::{Engine, ErrorKind, ExecuteError, LimitKind};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args().nth(1).ok_or("usage: bounded_http URL")?;
    // Illustrative for small requests; these values are not a whole-process RAM budget.
    let engine = Engine::builder()
        .max_connections(NonZeroUsize::new(4).ok_or("invalid connection count")?)
        .max_inflight_requests(NonZeroUsize::new(8).ok_or("invalid request count")?)
        .max_buffered_body_bytes(8 * 1024 * 1024)
        .build()?;
    let result = engine
        .get(url)
        .max_response_body_bytes(1024 * 1024)
        .total_timeout(Duration::from_secs(15))
        .call();
    if let Err(ExecuteError::Submission(error) | ExecuteError::Failed(error)) = &result {
        if error.kind() == ErrorKind::Limit
            && error.limit_kind() == Some(LimitKind::BufferedBodyBytes)
        {
            eprintln!("buffer allowance exhausted; release retained bodies or admit less work");
        }
    }
    engine.shutdown()?;
    let response = result?;
    println!(
        "HTTP {}, {} bytes",
        response.status(),
        response.body().len()
    );
    // On shared storage, copying remains an explicit application choice.
    let bytes = response
        .into_body()
        .try_into_vec()
        .unwrap_or_else(|shared| shared.as_bytes().to_vec());
    println!("application now owns {} bytes", bytes.len());
    Ok(())
}
