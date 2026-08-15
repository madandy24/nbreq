use std::cell::RefCell;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContextKind {
    Drive,
    Callback,
}

thread_local! {
    static ACTIVE: RefCell<Vec<(u64, ContextKind)>> = const { RefCell::new(Vec::new()) };
}

pub(crate) struct ContextGuard {
    engine_id: u64,
    kind: ContextKind,
}

impl ContextGuard {
    pub(crate) fn enter(engine_id: u64, kind: ContextKind) -> Self {
        ACTIVE.with(|active| active.borrow_mut().push((engine_id, kind)));
        Self { engine_id, kind }
    }
}

impl Drop for ContextGuard {
    fn drop(&mut self) {
        ACTIVE.with(|active| {
            let removed = active.borrow_mut().pop();
            debug_assert_eq!(removed, Some((self.engine_id, self.kind)));
        });
    }
}

pub(crate) fn is_active(engine_id: u64) -> bool {
    ACTIVE.with(|active| {
        active
            .borrow()
            .iter()
            .any(|(active_engine, _kind)| *active_engine == engine_id)
    })
}

pub(crate) fn is_callback(engine_id: u64) -> bool {
    ACTIVE.with(|active| {
        active.borrow().iter().any(|(active_engine, kind)| {
            *active_engine == engine_id && *kind == ContextKind::Callback
        })
    })
}
