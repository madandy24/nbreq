mod fixture;
mod soak;

use std::collections::HashMap;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
type Args = HashMap<String, String>;

fn argument<'a>(args: &'a Args, key: &str, default: &'a str) -> &'a str {
    args.get(key).map(String::as_str).unwrap_or(default)
}

fn emit(mut value: serde_json::Value) -> Result<()> {
    value["utc_ms"] = serde_json::json!(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis());
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &value)?;
    output.write_all(b"\n")?;
    output.flush()?;
    Ok(())
}

fn main() -> Result<()> {
    let mut input = std::env::args().skip(1);
    let mode = input.next().ok_or("expected fixture or soak")?;
    let mut args = Args::new();
    while let Some(key) = input.next() {
        let value = input.next().ok_or("missing argument value")?;
        if !key.starts_with("--") || args.insert(key, value).is_some() {
            return Err("invalid or duplicate argument".into());
        }
    }
    match mode.as_str() {
        "fixture" => fixture::run(&args),
        "soak" => soak::run(&args),
        _ => Err("unknown mode".into()),
    }
}
