use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const WAIT: Duration = Duration::from_secs(5);

pub struct Server {
    pub address: SocketAddr,
    worker: Option<JoinHandle<()>>,
}

impl Server {
    pub fn new(action: impl FnOnce(TcpStream) + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback listener");
        let address = listener.local_addr().expect("listener address");
        listener.set_nonblocking(true).expect("bounded accept");
        let worker = thread::spawn(move || {
            let until = Instant::now() + WAIT;
            let socket = loop {
                match listener.accept() {
                    Ok((socket, _)) => break socket,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < until, "consumer never connected");
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("accept failed: {error}"),
                }
            };
            // Windows can inherit the listener's nonblocking setting on an accepted socket.
            socket.set_nonblocking(false).expect("blocking fixture I/O");
            socket.set_read_timeout(Some(WAIT)).expect("read bound");
            socket.set_write_timeout(Some(WAIT)).expect("write bound");
            action(socket);
        });
        Self {
            address,
            worker: Some(worker),
        }
    }
    pub fn http(body: &'static [u8]) -> Self {
        Self::new(move |mut socket| {
            let head = read_head(&mut socket);
            assert!(head.starts_with("GET / "));
            reply(&mut socket, body);
        })
    }
    pub fn url(&self) -> String {
        format!("http://{}/", self.address)
    }
    pub fn finish(mut self) {
        self.worker
            .take()
            .expect("fixture worker")
            .join()
            .expect("fixture assertions");
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            // Fixture I/O is bounded even when a consumer assertion fails.
            let _ = worker.join();
        }
    }
}
pub fn read_head(socket: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        assert!(bytes.len() < 8192, "unbounded request head");
        let mut next = [0];
        socket.read_exact(&mut next).expect("request head");
        bytes.push(next[0]);
    }
    String::from_utf8(bytes).expect("ASCII fixture head")
}
pub fn reply(socket: &mut TcpStream, body: &[u8]) {
    write!(
        socket,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .expect("response head");
    socket.write_all(body).expect("response body");
}
