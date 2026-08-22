//! Feature-private entry points for the out-of-tree fuzz harnesses.

/// Exercises the production native buffered-response decoder under equivalent fragmentation
/// schedules and asserts that its terminal result and exact byte boundary do not change.
pub fn native_response_decoder(data: &[u8]) {
    crate::backend::fuzz_response_decoder(data);
}

/// Exercises the production streaming-response decoder under input-selected fragmentation and
/// reader backpressure without opening a network socket.
pub fn native_streaming_response_decoder(data: &[u8]) {
    if data.len() < 8 {
        return;
    }
    let (engine, _controller) = crate::testing::engine(crate::EngineConfig::manual())
        .expect("streaming decoder harness Engine must construct");
    let pending = engine
        .client()
        .submit(
            crate::Request::get("http://fuzz.invalid/")
                .build()
                .expect("streaming decoder harness request must build"),
        )
        .expect("streaming decoder harness request must submit");
    let handle = pending.handle();
    drop(pending);
    crate::backend::fuzz_streaming_response_decoder(data, handle);
    engine.cancel_all();
    engine
        .shutdown()
        .expect("streaming decoder harness Engine must stop");
}

/// Exercises the bounded native DNS response parser and its NBReq answer-policy invariants.
pub fn native_dns_response(data: &[u8]) {
    crate::backend::fuzz_dns_response(data);
}
