//! Bounded upload and incremental download: `cargo run --example A09-http-streaming -- [URL]`.
use std::io::Write;
use std::time::Duration;

use nbreq::{Engine, StreamRequest, UploadBody};

fn exchange(engine: &Engine, url: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (body, mut sender) = UploadBody::chunked(1024)?;
    let request = StreamRequest::post(url)
        .header("Content-Type", "text/plain")
        .body_stream(body)
        .max_request_body_bytes(1024)
        .max_response_body_bytes(64 * 1024)
        .total_timeout(Duration::from_secs(15))
        .build()?;
    let mut response = engine.client().submit_stream(request)?;
    // push() waits for upload capacity on a spawned Engine. finish() terminates the body.
    sender.push(b"hello ".to_vec())?;
    sender.push(b"from nbreq".to_vec())?;
    sender.finish()?;
    println!("HTTP {}", response.wait_head()?.status());

    // Process chunks directly without collecting the entire response. These are bytes: a UTF-8
    // character can straddle chunks, so do not decode each chunk as a separate string.
    let mut buffer = [0_u8; 1024];
    let mut total = 0;
    let mut output = std::io::stdout().lock();
    while let Some(count) = response.read(&mut buffer)? {
        output.write_all(&buffer[..count])?;
        total += count;
    }
    writeln!(output, "\nstreamed {total} response bytes")?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://httpbin.org/post".into());
    let engine = Engine::builder()
        .max_stream_queue_bytes_per_request(4096)
        .build()?;
    let result = exchange(&engine, &url);
    engine.shutdown()?;
    result
}
