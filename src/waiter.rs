//! Sealed waiter targets for [`Engine::drive_until`](crate::Engine::drive_until).

use crate::{
    Completion, PendingRequest, PendingResolve, PendingTcpConnect, ResolveCompletion,
    TcpConnectCompletion,
};

pub(crate) mod sealed {
    /// Crate-private polling and Engine identity.
    ///
    /// These are associated functions rather than methods, so downstream code cannot invoke them
    /// with method syntax. The trait is crate-private, so it cannot be named either.
    pub trait Sealed {
        fn try_output(this: &Self) -> Option<<Self as super::WaiterTarget>::Output>
        where
            Self: super::WaiterTarget;
        fn engine_id(this: &Self) -> u64;
    }
}

/// A finite accepted operation that a manual [`Engine`](crate::Engine) can drive to one terminal.
///
/// This trait is sealed. Only NBReq pending HTTP, DNS, and TCP types implement it. Each target
/// keeps its own completion type; there is no grab-bag terminal enum. Polling and Engine identity
/// stay on crate-private associated functions so callers cannot observe numeric Engine IDs.
/// Downstream generic code can use exactly `T: WaiterTarget`.
///
/// ```compile_fail
/// fn leak_output<T: nbreq::WaiterTarget>(pending: T) {
///     let _ = pending.try_output();
/// }
/// ```
///
/// ```compile_fail
/// fn leak_engine_id<T: nbreq::WaiterTarget>(pending: T) {
///     let _ = pending.engine_id();
/// }
/// ```
pub trait WaiterTarget: sealed::Sealed {
    /// Canonical terminal value produced when this waiter completes.
    type Output;
}

impl WaiterTarget for PendingRequest {
    type Output = Completion;
}

impl sealed::Sealed for PendingRequest {
    fn try_output(this: &Self) -> Option<Completion> {
        this.try_completion()
    }

    fn engine_id(this: &Self) -> u64 {
        this.request_id().engine
    }
}

impl WaiterTarget for PendingResolve {
    type Output = ResolveCompletion;
}

impl sealed::Sealed for PendingResolve {
    fn try_output(this: &Self) -> Option<ResolveCompletion> {
        this.try_completion()
    }

    fn engine_id(this: &Self) -> u64 {
        this.issued_engine_id()
    }
}

impl WaiterTarget for PendingTcpConnect {
    type Output = TcpConnectCompletion;
}

impl sealed::Sealed for PendingTcpConnect {
    fn try_output(this: &Self) -> Option<TcpConnectCompletion> {
        this.try_completion()
    }

    fn engine_id(this: &Self) -> u64 {
        this.issued_engine_id()
    }
}
