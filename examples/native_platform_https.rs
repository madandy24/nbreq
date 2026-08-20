//! Opt-in WP8 proof of host DNS and platform-trusted HTTPS.
//!
//! This deliberately uses NBReq's test-support constructor. It does not select the native backend
//! through `Engine::new` and is not a consumer API example.

use std::error::Error;
use std::io;
use std::time::Duration;

use nbreq::{EngineConfig, Request};

fn main() -> Result<(), Box<dyn Error>> {
    let url = std::env::args().nth(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "usage: native_platform_https https://platform-trusted-host/path",
        )
    })?;
    let engine = nbreq::testing::native_https_engine_with_system_dns(EngineConfig::spawned())?;
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
