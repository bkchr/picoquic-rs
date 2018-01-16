use error::*;

use std::net::SocketAddr;

use tokio_core::net::UdpSocket;
use tokio_core::reactor::Handle;

struct Server {
    socket: UdpSocket,
}

impl Server {
    pub fn new(listen_address: &SocketAddr, handle: &Handle) -> Result<Server, Error> {
        Ok(Server {
            socket: UdpSocket::bind(listen_address, handle)?,
        })
    }
}
