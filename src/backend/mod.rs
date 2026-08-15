//! Private transport boundary. Backend implementation types never enter the public API.

use std::time::Instant;

use crate::{DriveStatus, Error, ShutdownError};

#[cfg(feature = "curl")]
mod curl;
#[cfg(feature = "native")]
mod native;
mod scaffold;

pub(crate) trait Backend: Send {
    fn drive(&mut self, deadline: Instant) -> Result<DriveStatus, Error>;
    fn shutdown(&mut self) -> Result<(), ShutdownError>;
}

pub(crate) fn scaffold() -> Box<dyn Backend> {
    Box::new(scaffold::ScaffoldBackend::default())
}
