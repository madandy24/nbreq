#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    nbreq::fuzzing::native_streaming_response_decoder(data);
});
