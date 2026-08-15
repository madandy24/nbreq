//! Private owned-event dispatch boundary.

/// WP0's empty callback domain. WP1 replaces this with the bounded owned-event queue and workers.
#[derive(Default)]
pub(crate) struct ScaffoldDispatcher {
    sealed: bool,
}

impl ScaffoldDispatcher {
    pub(crate) fn seal(&mut self) {
        self.sealed = true;
    }
}
