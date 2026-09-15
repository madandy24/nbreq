//! Drive a lookup yourself: `cargo run --example B03-dns-manual -- [HOSTNAME]`.
use std::time::Duration;

use nbreq::{EngineBuilder, ResolveCompletion, ResolveRequest};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "example.com".into());
    let request = ResolveRequest::hostname(name)
        .total_timeout(Duration::from_secs(10))
        .build()?;
    let mut engine = EngineBuilder::manual().build()?;
    let pending = engine.resolver().submit(request)?;
    // Alternatively interleave engine.drive(deadline) with host events, as in A08.
    let result = engine.drive_until(pending);
    engine.shutdown()?;
    match result? {
        ResolveCompletion::Completed(answer) => {
            println!("DNS {:?}: {}", answer.status(), answer.name());
            for address in answer.addresses() {
                println!("{}", address.address());
            }
        }
        ResolveCompletion::Failed(error) => return Err(error.into()),
        ResolveCompletion::Cancelled => return Err("lookup cancelled".into()),
        _ => return Err("unrecognized resolution completion".into()),
    }
    Ok(())
}
