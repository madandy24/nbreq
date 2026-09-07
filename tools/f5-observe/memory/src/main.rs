mod client;
mod fixture;

use std::collections::HashMap;
use std::error::Error;
use std::io::{Read, Write};

#[cfg(feature = "alloc-meter")]
#[global_allocator]
static ALLOCATOR: nbreq_f5_meter::Metered<std::alloc::System> =
    nbreq_f5_meter::Metered::new(std::alloc::System);

type Result<T> = std::result::Result<T, Box<dyn Error>>;
type Args = HashMap<String, String>;

fn argument<'a>(args: &'a Args, key: &str, default: &'a str) -> &'a str {
    args.get(key).map(String::as_str).unwrap_or(default)
}

fn emit(value: serde_json::Value) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &value)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

fn acknowledge() -> Result<()> {
    let mut byte = [0];
    std::io::stdin().lock().read_exact(&mut byte)?;
    if byte != *b"\n" {
        return Err("invalid controller acknowledgement".into());
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut input = std::env::args().skip(1);
    let mode = input
        .next()
        .ok_or("expected fixture, client or verified subcommand")?;
    let mut args = Args::new();
    while let Some(key) = input.next() {
        if !key.starts_with("--") {
            return Err(format!("invalid argument {key}").into());
        }
        let value = input
            .next()
            .ok_or_else(|| format!("missing value for {key}"))?;
        if args.insert(key.clone(), value).is_some() {
            return Err(format!("duplicate argument {key}").into());
        }
    }
    match mode.as_str() {
        "fixture" => fixture::run(&args),
        "client" => client::run(&args),
        "verified" => client::verified(&args),
        _ => Err(format!("unknown subcommand {mode}").into()),
    }
}
