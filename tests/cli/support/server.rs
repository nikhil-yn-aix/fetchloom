//! owned listeners and connections, stopped and joined when their test server goes away.

use std::io;
use std::net::{Shutdown as SocketShutdown, TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

#[derive(Default)]
pub(super) struct Shutdown {
    stopped: Mutex<bool>,
    changed: Condvar,
    sockets: Mutex<Vec<Option<TcpStream>>>,
}

pub(super) struct Connection {
    shutdown: Arc<Shutdown>,
    index: usize,
}

impl Drop for Connection {
    fn drop(&mut self) {
        let stream = self
            .shutdown
            .sockets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)[self.index]
            .take();
        if let Some(stream) = stream {
            // windows needs an explicit FIN before duplicated socket handles are closed.
            let _ = stream.shutdown(SocketShutdown::Write);
        }
    }
}

impl Shutdown {
    pub(super) fn stopped(&self) -> bool {
        *self.stopped.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(super) fn track(self: &Arc<Self>, stream: &TcpStream) -> Option<Connection> {
        let stopped = self.stopped.lock().unwrap_or_else(PoisonError::into_inner);
        if *stopped {
            return None;
        }
        let mut sockets = self.sockets.lock().unwrap_or_else(PoisonError::into_inner);
        let index = sockets.len();
        sockets.push(Some(stream.try_clone().ok()?));
        Some(Connection {
            shutdown: self.clone(),
            index,
        })
    }

    pub(super) fn accept(
        self: &Arc<Self>,
        listener: &TcpListener,
    ) -> Option<(TcpStream, Connection)> {
        while !self.stopped() {
            match listener.accept() {
                Ok((stream, _)) => {
                    // accepted sockets inherit nonblocking mode on some platforms.
                    stream.set_nonblocking(false).ok()?;
                    let connection = self.track(&stream)?;
                    return Some((stream, connection));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if self.pause(Duration::from_millis(5)) {
                        return None;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => return None,
            }
        }
        None
    }

    /// a paced response can be interrupted immediately by server shutdown.
    pub(super) fn pause(&self, duration: Duration) -> bool {
        let stopped = self.stopped.lock().unwrap_or_else(PoisonError::into_inner);
        let (stopped, _) = self
            .changed
            .wait_timeout_while(stopped, duration, |stopped| !*stopped)
            .unwrap_or_else(PoisonError::into_inner);
        *stopped
    }

    fn stop(&self) {
        *self.stopped.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.changed.notify_all();
        for stream in self
            .sockets
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .flatten()
        {
            let _ = stream.shutdown(SocketShutdown::Both);
        }
    }
}

pub(super) struct ServerTask {
    shutdown: Arc<Shutdown>,
    worker: Option<JoinHandle<()>>,
}

impl ServerTask {
    pub(super) fn start(
        listener: TcpListener,
        run: impl FnOnce(TcpListener, Arc<Shutdown>) + Send + 'static,
    ) -> io::Result<Self> {
        listener.set_nonblocking(true)?;
        let shutdown = Arc::new(Shutdown::default());
        let shared = shutdown.clone();
        let worker = std::thread::spawn(move || run(listener, shared));
        Ok(Self {
            shutdown,
            worker: Some(worker),
        })
    }
}

impl Drop for ServerTask {
    fn drop(&mut self) {
        self.shutdown.stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
