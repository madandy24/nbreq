//! Submit a configured lookup: `cargo run --example B02-dns-nonblocking -- [HOSTNAME]`.
use std::time::Duration;

use nbreq::{
    AddressFamily, AddressOrder, CacheMode, Engine, ResolveCompletion, ResolveRequest,
    ResolveWaitOutcome,
};

fn lookup(engine: &Engine, name: &str) -> Result<(), Box<dyn std::error::Error>> {
    let request = ResolveRequest::hostname(name)
        .address_family(AddressFamily::Both)
        .address_order(AddressOrder::Ipv6ThenIpv4)
        .cache_mode(CacheMode::Use)
        .max_results(16)
        .use_search_suffixes(false) // Exact name; search expansion is an explicit opt-in.
        .total_timeout(Duration::from_secs(10))
        .build()?;
    let pending = engine.resolver().submit(request)?;
    println!("lookup submitted; ready now: {}", pending.is_complete());
    let completion = match pending.wait_for(Duration::from_millis(1)) {
        ResolveWaitOutcome::Completed(completion) => completion,
        ResolveWaitOutcome::TimedOut(pending) => {
            // This was a local wait timeout, not a DNS failure. The request is still live.
            println!("local wait elapsed; doing other work before collecting the answer");
            // To abandon it instead: pending.handle().cancel()?; then observe its completion.
            pending.wait()
        }
        _ => return Err("unrecognized wait outcome".into()),
    };
    match completion {
        ResolveCompletion::Completed(answer) => {
            println!(
                "DNS {:?}: {} (cached: {})",
                answer.status(),
                answer.name(),
                answer.from_cache()
            );
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "example.com".into());
    let engine = Engine::builder().build()?;
    let result = lookup(&engine, &name);
    engine.shutdown()?;
    result
}
