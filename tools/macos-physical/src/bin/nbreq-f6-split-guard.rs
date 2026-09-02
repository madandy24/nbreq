use std::error::Error;

use nbreq::{Engine, EngineConfig, ErrorKind};

fn main() -> Result<(), Box<dyn Error>> {
    match Engine::new(EngineConfig::spawned()) {
        Err(error) if error.kind() == ErrorKind::Unsupported => {
            println!("SPLIT_DNS_REJECTED kind={:?} message={error}", error.kind());
            Ok(())
        }
        Err(error) => Err(format!(
            "split-DNS construction failed with {:?}, expected Unsupported: {error}",
            error.kind()
        )
        .into()),
        Ok(engine) => {
            engine.shutdown()?;
            Err("SPLIT_DNS_WAS_ACCEPTED: ordinary Engine construction ignored the supplemental resolver".into())
        }
    }
}
