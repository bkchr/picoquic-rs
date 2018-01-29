use error::*;
use connection::Connection;
use config::Config;
use context_inner::{Connect, ConnectFuture, ContextInner };

use std::net::SocketAddr;

use tokio_core::reactor::Handle;

use futures::sync::mpsc::{UnboundedReceiver};
use futures::{Poll, Stream};

pub struct Context {
    recv_con: UnboundedReceiver<Connection>,
    local_addr: SocketAddr,
    connect: Connect,
}

impl Context {
    pub fn new(
        listen_address: &SocketAddr,
        handle: &Handle,
        config: Config,
    ) -> Result<Context, Error> {
        let (inner, recv_con, connect) = ContextInner::new(listen_address, handle, config)?;

        let local_addr = inner.local_addr();

        // start the inner future
        handle.spawn(inner);

        Ok(Context {
            recv_con,
            local_addr,
            connect,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    pub fn connect_to(&mut self, addr: SocketAddr) -> ConnectFuture {
        self.connect.connect_to(addr)
    }

    pub fn get_connect(&self) -> Connect {
        self.connect.clone()
    }
}

impl Stream for Context {
    type Item = Connection;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.recv_con.poll().map_err(|_| ErrorKind::Unknown.into())
    }
}
