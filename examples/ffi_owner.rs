use std::time::Duration;

use nbreq::{Client, Engine, EngineConfig, ShutdownOutcome};

// A future FFI adapter can keep these behind opaque host-language handles. The unique Engine owner
// remains separate from cheap Client command handles.
struct Service {
    engine: Option<Engine>,
    client: Client,
}

impl Service {
    fn new() -> Result<Self, nbreq::Error> {
        let engine = Engine::new(EngineConfig::spawned())?;
        let client = engine.client();
        Ok(Self {
            engine: Some(engine),
            client,
        })
    }

    fn client(&self) -> Client {
        self.client.clone()
    }

    fn stop(&mut self) -> Result<(), nbreq::ShutdownError> {
        if let Some(engine) = self.engine.take() {
            match engine.shutdown_for(Duration::ZERO)? {
                ShutdownOutcome::Complete => {}
                ShutdownOutcome::CallbacksRemaining(callbacks) => callbacks.wait()?,
                _ => {}
            }
        }
        Ok(())
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut service = Service::new()?;
    let client = service.client();
    drop(client);
    service.stop()?;
    Ok(())
}
