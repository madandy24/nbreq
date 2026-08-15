use std::time::{Duration, Instant};

use nbreq::{DriveStatus, EngineBuilder, Request};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut engine = EngineBuilder::manual().build()?;
    let client = engine.client();
    let request = Request::post("https://example.invalid/")
        .body(b"buffered body".to_vec())
        .build()?;

    let _pending = client.submit(request);
    let _status: DriveStatus = engine.drive(Instant::now() + Duration::from_millis(10))?;
    engine.cancel_all();
    engine.shutdown()?;
    Ok(())
}
