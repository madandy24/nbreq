//! Resolve a hostname: `cargo run --example B01-dns-blocking -- [HOSTNAME]`.
use std::time::Duration;

use nbreq::{Engine, ResolveRequest, ResolveStatus};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "example.com".into());
    let request = ResolveRequest::hostname(name)
        .total_timeout(Duration::from_secs(10))
        .build()?;
    let engine = Engine::builder().build()?;
    let result = engine.resolver().execute(request);
    engine.shutdown()?;
    let answer = result?;
    println!("DNS {:?}: {}", answer.status(), answer.name());
    match answer.status() {
        ResolveStatus::Answer => {
            for address in answer.addresses() {
                println!("{}", address.address());
            }
        }
        ResolveStatus::NameNotFound => println!("name does not exist"),
        ResolveStatus::NoData => println!("no addresses in the requested families"),
        _ => return Err("unrecognized resolution status".into()),
    }
    // A valid negative DNS answer is not a transport failure.
    Ok(())
}
