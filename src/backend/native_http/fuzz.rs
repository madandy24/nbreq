//! Coverage-guided harnesses for the native HTTP/1.1 response decoders.

use super::HttpLimits;
use super::http1::{ResponseDecoder, StreamDecodeProgress, StreamingResponseDecoder};
use crate::{Error, Response};

#[cfg(any(test, fuzzing))]
#[derive(Debug, Eq, PartialEq)]
enum FuzzDecodeOutcome {
    Complete {
        response: Response,
        consumed: usize,
        permits_reuse: bool,
    },
    Failed(Error),
    Eof {
        response: Option<Response>,
        permits_reuse: bool,
    },
}

#[cfg(any(test, fuzzing))]
fn fuzz_decode_with<F>(
    wire: &[u8],
    response_to_head: bool,
    limits: HttpLimits,
    mut next_chunk: F,
) -> FuzzDecodeOutcome
where
    F: FnMut(usize) -> usize,
{
    let mut decoder = ResponseDecoder::new(response_to_head, limits);
    let mut offset = 0;
    while offset < wire.len() {
        let remaining = wire.len() - offset;
        let chunk_len = next_chunk(remaining).clamp(1, remaining);
        match decoder.ingest(&wire[offset..offset + chunk_len]) {
            Ok(progress) => {
                if let Some(response) = progress.response {
                    return FuzzDecodeOutcome::Complete {
                        response,
                        consumed: offset + progress.consumed,
                        permits_reuse: progress.permits_reuse,
                    };
                }
                offset += progress.consumed;
            }
            Err(error) => return FuzzDecodeOutcome::Failed(error),
        }
    }
    let permits_reuse = decoder.permits_reuse;
    match decoder.eof() {
        Ok(response) => FuzzDecodeOutcome::Eof {
            response,
            permits_reuse,
        },
        Err(error) => FuzzDecodeOutcome::Failed(error),
    }
}

/// Coverage-guided entry point used by `fuzz/fuzz_targets/native_response_decoder.rs`.
///
/// The first six bytes select bounded parser limits and an irregular fragmentation schedule. The
/// remainder is the HTTP wire image. Comparing three schedules turns arbitrary mutation into a
/// state-machine oracle instead of merely checking that the parser does not panic.
#[cfg(any(test, fuzzing))]
pub(crate) fn fuzz_response_decoder(data: &[u8]) {
    const CONTROL_BYTES: usize = 6;
    if data.len() < CONTROL_BYTES {
        return;
    }
    let control = &data[..CONTROL_BYTES];
    let encoded_wire = &data[CONTROL_BYTES..];
    let decoded_wire;
    let wire = if control[0] & 2 != 0 {
        decoded_wire = decode_fuzz_seed_escapes(encoded_wire);
        decoded_wire.as_slice()
    } else {
        encoded_wire
    };
    let response_to_head = control[0] & 1 != 0;
    let limits = HttpLimits {
        body_bytes: usize::from(control[1]).saturating_mul(64),
        header_bytes: usize::from(control[2]).saturating_mul(16).max(1),
        header_count: usize::from(control[3] % 64).saturating_add(1),
    };

    let whole = fuzz_decode_with(wire, response_to_head, limits, |remaining| remaining);
    let bytewise = fuzz_decode_with(wire, response_to_head, limits, |_| 1);
    assert_eq!(
        whole, bytewise,
        "native response decoding changed under bytewise fragmentation"
    );

    let mut turn = 0_usize;
    let irregular = fuzz_decode_with(wire, response_to_head, limits, |remaining| {
        let selector = control[4 + (turn & 1)];
        turn = turn.wrapping_add(1);
        usize::from(selector % 64).saturating_add(1).min(remaining)
    });
    assert_eq!(
        whole, irregular,
        "native response decoding changed under irregular fragmentation"
    );
}

#[cfg(any(test, fuzzing))]
fn decode_fuzz_seed_escapes(bytes: &[u8]) -> Vec<u8> {
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 1 < bytes.len() {
            match bytes[index + 1] {
                b'r' => {
                    decoded.push(b'\r');
                    index += 2;
                    continue;
                }
                b'n' => {
                    decoded.push(b'\n');
                    index += 2;
                    continue;
                }
                b'\\' => {
                    decoded.push(b'\\');
                    index += 2;
                    continue;
                }
                _ => {}
            }
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    decoded
}

/// Coverage-guided entry point for the streaming decoder and its reader backpressure boundary.
#[cfg(any(test, fuzzing))]
pub(crate) fn fuzz_streaming_response_decoder(data: &[u8], handle: crate::RequestHandle) {
    const CONTROL_BYTES: usize = 8;
    if data.len() < CONTROL_BYTES {
        return;
    }
    let control = &data[..CONTROL_BYTES];
    let encoded_wire = &data[CONTROL_BYTES..];
    let decoded_wire;
    let wire = if control[0] & 2 != 0 {
        decoded_wire = decode_fuzz_seed_escapes(encoded_wire);
        decoded_wire.as_slice()
    } else {
        encoded_wire
    };
    let deliver = control[0] & 4 != 0;
    let limits = HttpLimits {
        body_bytes: usize::from(control[1]).saturating_mul(64),
        header_bytes: usize::from(control[2]).saturating_mul(16).max(1),
        header_count: usize::from(control[3] % 64).saturating_add(1),
    };
    let queue_capacity = usize::from(control[4] % 64).saturating_add(1);
    let (mut reader, sink, _control) = crate::stream::response_pair(
        handle,
        crate::RunMode::Manual,
        queue_capacity,
        limits.body_bytes,
        None,
        None,
    )
    .expect("streaming decoder fuzz response pair must construct");
    let mut decoder = StreamingResponseDecoder::new(control[0] & 1 != 0, limits, sink);
    let mut offset = 0_usize;
    let mut turn = 0_usize;

    while offset < wire.len() {
        let remaining = wire.len() - offset;
        let selector = control[5 + (turn & 1)];
        turn = turn.wrapping_add(1);
        let chunk_len = usize::from(selector % 64).saturating_add(1).min(remaining);
        let progress = match decoder.ingest(&wire[offset..offset + chunk_len]) {
            Ok(progress) => progress,
            Err(error) => {
                decoder.fail(error);
                return;
            }
        };
        match progress {
            StreamDecodeProgress::Head { consumed, .. } => {
                assert!(consumed != 0 && consumed <= chunk_len);
                offset += consumed;
                let decision = match decoder.decide_head(deliver) {
                    Ok(decision) => decision,
                    Err(error) => {
                        decoder.fail(error);
                        return;
                    }
                };
                if decision.complete {
                    finish_fuzz_stream_decoder(decoder, deliver);
                    return;
                }
            }
            StreamDecodeProgress::Body { consumed, blocked } => {
                assert!(consumed <= chunk_len);
                offset += consumed;
                if blocked {
                    assert!(deliver, "discarded streaming response became backpressured");
                    let mut destination = vec![
                        0_u8;
                        usize::from(control[7] % 64)
                            .saturating_add(1)
                            .min(queue_capacity)
                    ];
                    match reader.try_read(&mut destination) {
                        Ok(crate::StreamRead::Data(read)) => {
                            assert!(read != 0, "backpressured reader released no capacity");
                        }
                        Ok(crate::StreamRead::Pending) => {
                            panic!("backpressured decoder had no reader-visible bytes")
                        }
                        Ok(crate::StreamRead::Eof) => {
                            panic!("backpressured decoder reached reader EOF")
                        }
                        Err(_) => return,
                    }
                } else {
                    assert!(consumed != 0, "streaming decoder made no progress");
                }
            }
            StreamDecodeProgress::Complete {
                consumed,
                delivered,
                ..
            } => {
                assert!(consumed != 0 && consumed <= chunk_len);
                assert_eq!(delivered, deliver);
                finish_fuzz_stream_decoder(decoder, deliver);
                return;
            }
        }
    }

    match decoder.eof() {
        Ok(Some((_permits_reuse, delivered))) => {
            assert_eq!(delivered, deliver);
            finish_fuzz_stream_decoder(decoder, deliver);
        }
        Ok(None) => {}
        Err(error) => decoder.fail(error),
    }
}

#[cfg(any(test, fuzzing))]
fn finish_fuzz_stream_decoder(decoder: StreamingResponseDecoder, delivered: bool) {
    if !delivered {
        let mut sink = decoder
            .into_response()
            .expect("discarded complete response must return its sink");
        sink.cancel();
    }
}
