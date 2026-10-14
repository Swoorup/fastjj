//! Bridge a libssh2 direct-tcpip channel using a worker thread, never a helper.
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::Result;
use ssh2::Session;

pub struct Bridge {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}
impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn connect(session: Session, host: &str, port: u16) -> Result<(UnixStream, Bridge)> {
    let mut channel = session.channel_direct_tcpip(host, port, None)?;
    let (client, mut socket) = UnixStream::pair()?;
    socket.set_nonblocking(true)?;
    session.set_blocking(false);
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let worker = thread::Builder::new()
        .name("ssh-tunnel".into())
        .spawn(move || {
            let mut to_remote = Buffer::default();
            let mut to_client = Buffer::default();
            let mut local_eof = false;
            let mut remote_eof = false;
            let mut sent_eof = false;
            while !flag.load(Ordering::Acquire) {
                let mut progress = false;
                if !local_eof {
                    match to_remote.receive(&mut socket) {
                        Ok((eof, moved)) => {
                            local_eof = eof;
                            progress |= moved;
                        }
                        Err(_) => break,
                    }
                }
                if !remote_eof {
                    match to_client.receive(&mut channel) {
                        Ok((eof, moved)) => {
                            remote_eof = eof;
                            progress |= moved;
                        }
                        Err(_) => break,
                    }
                }
                match to_remote.send(&mut channel) {
                    Ok(moved) => progress |= moved,
                    Err(_) => break,
                }
                match to_client.send(&mut socket) {
                    Ok(moved) => progress |= moved,
                    Err(_) => break,
                }
                if local_eof && to_remote.empty() && !sent_eof {
                    sent_eof = channel.send_eof().is_ok();
                }
                if remote_eof && to_client.empty() {
                    let _ = socket.shutdown(Shutdown::Write);
                    if local_eof {
                        break;
                    }
                }
                if !progress {
                    thread::sleep(Duration::from_millis(2));
                }
            }
            let _ = socket.shutdown(Shutdown::Both);
        })?;
    Ok((
        client,
        Bridge {
            stop,
            worker: Some(worker),
        },
    ))
}

#[derive(Default)]
struct Buffer {
    data: Vec<u8>,
    position: usize,
}
impl Buffer {
    fn empty(&self) -> bool {
        self.position == self.data.len()
    }
    fn receive(&mut self, source: &mut impl Read) -> io::Result<(bool, bool)> {
        if !self.empty() {
            return Ok((false, false));
        }
        self.data.resize(65536, 0);
        self.position = 0;
        match source.read(&mut self.data) {
            Ok(size) => {
                self.data.truncate(size);
                Ok((size == 0, size != 0))
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                self.data.clear();
                Ok((false, false))
            }
            Err(error) => Err(error),
        }
    }
    fn send(&mut self, target: &mut impl Write) -> io::Result<bool> {
        if self.empty() {
            return Ok(false);
        }
        match target.write(&self.data[self.position..]) {
            Ok(0) => Err(io::ErrorKind::WriteZero.into()),
            Ok(size) => {
                self.position += size;
                Ok(true)
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
}
