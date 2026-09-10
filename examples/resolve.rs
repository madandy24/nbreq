//! Run with `cargo run --example resolve -- example.com`.
use std::time::Duration;

use nbreq::{Engine, ResolveRequest, ResolveStatus};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let name = std::env::args().nth(1).ok_or("usage: resolve HOSTNAME")?;
    let engine = Engine::builder().build()?;
    let request = ResolveRequest::hostname(name)
        .total_timeout(Duration::from_secs(10))
        .build()?;
    let result = engine.resolver().execute(request);
    engine.shutdown()?;
    let answer = result?;
    match answer.status() {
        ResolveStatus::Answer => {
            for address in answer.addresses() {
                println!("{}", address.address());
            }
        }
        ResolveStatus::NameNotFound => println!("name does not exist"),
        ResolveStatus::NoData => println!("no addresses in the requested families"),
        _ => println!("unrecognized resolution status"),
    }
    Ok(())
}
