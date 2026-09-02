use std::error::Error;
use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const DEFAULT_URLS: &str = "https://gds.caverock.com/,https://gds2.caverock.com/";

pub fn env_duration(name: &str, default_seconds: u64) -> Result<Duration, Box<dyn Error>> {
    let seconds = match std::env::var(name) {
        Ok(value) => value.parse::<u64>()?,
        Err(std::env::VarError::NotPresent) => default_seconds,
        Err(error) => return Err(error.into()),
    };
    if seconds == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{name} is zero")).into());
    }
    Ok(Duration::from_secs(seconds))
}

pub fn env_u16(name: &str, default: u16) -> Result<u16, Box<dyn Error>> {
    let value = match std::env::var(name) {
        Ok(value) => value.parse::<u16>()?,
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => return Err(error.into()),
    };
    if value == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{name} is zero")).into());
    }
    Ok(value)
}

pub fn urls_from_env() -> Result<Vec<String>, Box<dyn Error>> {
    let value = std::env::var("NBREQ_SOAK_URLS").unwrap_or_else(|_| DEFAULT_URLS.to_owned());
    let urls: Vec<String> = value
        .split(',')
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
        .collect();
    if urls.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NBREQ_SOAK_URLS contains no URLs",
        )
        .into());
    }
    Ok(urls)
}

pub fn event(message: impl AsRef<str>) {
    println!("{} {}", timestamp(), message.as_ref());
}

fn timestamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
}
