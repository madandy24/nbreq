//! Feature-private entry points for the out-of-tree fuzz harnesses.

/// Exercises the production native buffered-response decoder under equivalent fragmentation
/// schedules and asserts that its terminal result and exact byte boundary do not change.
pub fn native_response_decoder(data: &[u8]) {
    crate::backend::fuzz_response_decoder(data);
}
