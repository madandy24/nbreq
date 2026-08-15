use std::time::Instant;

use crate::{DriveStatus, Error, ShutdownError};

use super::Backend;

#[derive(Default)]
pub(super) struct ScaffoldBackend {
    stopped: bool,
}

impl Backend for ScaffoldBackend {
    fn drive(&mut self, deadline: Instant) -> Result<DriveStatus, Error> {
        if Instant::now() >= deadline {
            Ok(DriveStatus::DeadlineReached)
        } else {
            Ok(DriveStatus::Idle)
        }
    }

    fn shutdown(&mut self) -> Result<(), ShutdownError> {
        self.stopped = true;
        Ok(())
    }
}
