use error::*;
use connection::Connection;
use config::Config;
use context_inner::{ContextInner, NewConnectionFuture, NewConnectionHandle};

use std::net::SocketAddr;

use tokio_core::reactor::Handle;

use futures::sync::mpsc::UnboundedReceiver;
use futures::{Poll, Stream};

/// The `Picoquic` context. It setups and controls the `UdpSocket`. Every incoming `Connection`
/// can be obtained by polling this context.
pub struct Context {
    recv_con: UnboundedReceiver<Connection>,
    local_addr: SocketAddr,
    new_connection_handle: NewConnectionHandle,
}

impl Context {
    /// Creates a new `Context`.
    pub fn new(
        listen_address: &SocketAddr,
        handle: &Handle,
        config: Config,
    ) -> Result<Context, Error> {
        let (inner, recv_con, new_connection_handle) =
            ContextInner::new(listen_address, handle, config)?;

        let local_addr = inner.local_addr();

        // start the inner future
        handle.spawn(inner);

        Ok(Context {
            recv_con,
            local_addr,
            new_connection_handle,
        })
    }

    /// Returns the local address, this `Context` is bound to.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Connects to the given address and returns a future that resolves into a `Connection`.
    pub fn new_connection(&mut self, addr: SocketAddr) -> NewConnectionFuture {
        self.new_connection_handle.new_connection(addr)
    }

    /// Returns the handle to create new connections.
    pub fn get_new_connection_handle(&self) -> NewConnectionHandle {
        self.new_connection_handle.clone()
    }
}

impl Stream for Context {
    type Item = Connection;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.recv_con.poll().map_err(|_| ErrorKind::Unknown.into())
    }
}
