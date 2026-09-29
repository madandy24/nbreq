//! Consumer compilation of the 0.3 optional-timeout API.
use nbreq::{Request, StreamRequest, TcpConnectRequest};
use std::time::Duration;

#[test]
fn optional_timeout_api_and_stream_conversion_work_for_consumers() {
    let request = Request::get("https://example.test/")
        .connect_timeout(Duration::from_secs(7))
        .inactivity_timeout(Some(Duration::from_secs(8)))
        .total_timeout(None)
        .build()
        .expect("HTTP optional timeout setters accept valid inputs");
    assert_eq!(
        request.options().connect_timeout,
        Some(Duration::from_secs(7))
    );
    let stream = StreamRequest::from(request);
    assert_eq!(
        stream.options().inactivity_timeout,
        Some(Duration::from_secs(8))
    );
    assert_eq!(stream.options().total_timeout, None);

    let tcp = TcpConnectRequest::hostname("example.test", 443)
        .connect_timeout(None)
        .write_inactivity_timeout(None)
        .build()
        .expect("TCP optional timeout setters accept valid inputs");
    assert_eq!(tcp.connect_timeout(), None);
    assert_eq!(tcp.read_inactivity_timeout(), None);
    assert_eq!(tcp.write_inactivity_timeout(), None);
}
