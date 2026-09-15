//! A bounded one-client loopback echo server. NBReq client code stays in the C examples.
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub struct Server {
    address: SocketAddr,
    stop: Sender<()>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

impl Server {
    pub fn start() -> io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let (stop, stopping) = mpsc::channel();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
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
            stream.set_write_timeout(Some(Duration::from_secs(5)))?;
            let mut buffer = [0_u8; 1024];
            let mut total = 0;
            loop {
                if Instant::now() >= deadline {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                let count = stream.read(&mut buffer)?;
                if count == 0 {
                    return Ok(());
                } // Peer half-close; drop sends our EOF.
                total += count;
                if total > 64 * 1024 {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                stream.write_all(&buffer[..count])?;
            }
        });
        Ok(Self {
            address,
            stop,
            worker: Some(worker),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn stop(mut self) -> io::Result<()> {
        self.join()
    }

    fn join(&mut self) -> io::Result<()> {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| io::Error::other("echo server panicked"))??;
        }
        Ok(())
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.join();
    }
}
