use std::io::{Read, Write};
use std::net::TcpListener;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use nbreq::{Completion, EngineConfig, Request, testing};

#[unsafe(no_mangle)]
pub extern "C" fn nbreq_curl_dll_probe() -> i32 {
    match catch_unwind(AssertUnwindSafe(run_probe)) {
        Ok(Ok(())) => 0,
        Ok(Err(code)) => code,
        Err(_) => 100,
    }
}

fn run_probe() -> Result<(), i32> {
    let listener = TcpListener::bind("127.0.0.1:0").map_err(|_| 1)?;
    let address = listener.local_addr().map_err(|_| 2)?;
    let server = thread::spawn(move || -> Result<(), i32> {
        let (mut stream, _peer) = listener.accept().map_err(|_| 3)?;
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .map_err(|_| 4)?;
        let mut request = Vec::new();
        let mut chunk = [0_u8; 1024];
        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut chunk).map_err(|_| 5)?;
            if read == 0 {
                return Err(6);
            }
            request.extend_from_slice(&chunk[..read]);
        }
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\ndll probe",
            )
            .map_err(|_| 7)
    });

    // This is the first curl use. NBReq's patched binding initializes libcurl here, on the
    // spawned reactor thread, after this exported function has been called--never in DllMain.
    let engine = testing::curl_engine(EngineConfig::spawned()).map_err(|_| 8)?;
    let client = engine.client();
    let request = Request::get(format!("http://{address}/"))
        .build()
        .map_err(|_| 9)?;
    let (terminal_tx, terminal_rx) = mpsc::channel();
    client
        .start(request, move |completion| {
            let valid = matches!(
                completion,
                Completion::Completed(response)
                    if response.status() == 200 && response.body() == b"dll probe"
            );
            let _ignored = terminal_tx.send(valid);
        })
        .map_err(|_| 10)?;
    if !terminal_rx
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_| 11)?
    {
        return Err(12);
    }
    engine.shutdown().map_err(|_| 13)?;
    server.join().map_err(|_| 14)??;
    Ok(())
}
