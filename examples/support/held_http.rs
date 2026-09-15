//! Local fixture plumbing only; the NBReq cancellation example is in A07.
use std::io::{self, Read};
use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub struct Server {
    address: SocketAddr,
    received: Receiver<()>,
    stop: Sender<()>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

impl Server {
    pub fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let (ack, received) = mpsc::channel();
        let (stop, stopping) = mpsc::channel();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut stream = loop {
                if stopping.try_recv().is_ok() {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => return Err(error),
                }
            };
            // Accepted sockets can inherit the listener's nonblocking mode on Windows.
            stream.set_nonblocking(false)?;
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                if header.len() >= 8192 || Instant::now() >= deadline {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                let mut byte = [0];
                stream.read_exact(&mut byte)?;
                header.push(byte[0]);
            }
            let _ = ack.send(());
            // Keep the socket open without responding until the example has checked Cancelled.
            stopping
                .recv_timeout(Duration::from_secs(20))
                .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error))?;
            Ok(())
        });
        Ok(Self {
            address,
            received,
            stop,
            worker: Some(worker),
        })
    }

    pub fn url(&self) -> String {
        format!("http://{}/held", self.address)
    }

    pub fn wait_for_request(&self) -> Result<(), mpsc::RecvTimeoutError> {
        self.received.recv_timeout(Duration::from_secs(10))
    }

    pub fn stop(mut self) -> io::Result<()> {
        self.join()
    }

    fn join(&mut self) -> io::Result<()> {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("held HTTP server panicked"))??;
        }
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.join();
    }
}
