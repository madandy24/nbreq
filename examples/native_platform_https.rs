//! Ordinary-constructor proof of host DNS and platform-trusted HTTPS.

use std::error::Error;
use std::io;
use std::time::Duration;

use nbreq::{Engine, EngineConfig, Request};

fn main() -> Result<(), Box<dyn Error>> {
    let url = std::env::args().nth(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: native_platform_https https://platform-trusted-host/path",
        )
    })?;
    let engine = Engine::new(EngineConfig::spawned())?;
    let response = engine.client().execute(
        Request::get(url)
            .total_timeout(Duration::from_secs(15))
            .build()?,
    )?;
    println!(
        "platform_https_status={} body_bytes={}",
        response.status(),
        response.body().len()
    );
    engine.shutdown()?;
    Ok(())
}
