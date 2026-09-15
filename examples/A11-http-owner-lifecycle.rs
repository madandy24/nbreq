//! Embed an Engine in a service: `cargo run --example A11-http-owner-lifecycle -- [URL]`.
use std::time::Duration;

use nbreq::{Client, Completion, Engine, ErrorKind, Request, ShutdownOutcome};

// An FFI adapter can put this owner behind an opaque host-language handle.
struct Service {
    engine: Option<Engine>,
    client: Client,
}

impl Service {
    fn new() -> Result<Self, nbreq::Error> {
        let engine = Engine::builder().build()?;
        let client = engine.client();
        Ok(Self {
            engine: Some(engine),
            client,
        })
    }

    fn client(&self) -> Client {
        self.client.clone()
    }

    fn stop(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(engine) = self.engine.take() {
            // Stops admission and cancels outstanding work. A zero callback-wait allowance can
            // return a join handle; a real host can retain it and wait at a suitable boundary.
            match engine.shutdown_for(Duration::ZERO)? {
                ShutdownOutcome::Complete => {}
                ShutdownOutcome::CallbacksRemaining(callbacks) => callbacks.wait()?,
                _ => return Err("unrecognized shutdown outcome".into()),
            }
        }
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://httpbin.org/get".into());
    let request = Request::get(&url)
        .total_timeout(Duration::from_secs(15))
        .build()?;
    let mut service = Service::new()?;
    let client = service.client();
    let result = client.submit(request)?.wait();
    service.stop()?;
    service.stop()?; // Idempotent at our service boundary.
    match result {
        Completion::Completed(response) => println!("HTTP {}", response.status()),
        Completion::Failed(error) => return Err(error.into()),
        Completion::Cancelled => return Err("request cancelled".into()),
        _ => return Err("unrecognized completion".into()),
    }
    // Retaining a Client did not retain the Engine's sockets or workers.
    let rejected = client
        .submit(Request::get(url).build()?)
        .expect_err("owner has stopped");
    assert_eq!(rejected.kind(), ErrorKind::EngineStopped);
    println!("detached Client rejects new work: EngineStopped (verified)");
    Ok(())
}
