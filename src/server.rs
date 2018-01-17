use error::*;
use stream;

use picoquic_sys::picoquic::{picoquic_cnx_t, picoquic_call_back_event_t};

use std::net::SocketAddr;
use std::os::raw::c_void;

use tokio_core::net::UdpSocket;
use tokio_core::reactor::Handle;

use libc::size_t;


struct Server {
    socket: UdpSocket,
}

impl Server {
    pub fn new(listen_address: &SocketAddr, handle: &Handle) -> Result<Server, Error> {
        Ok(Server {
            socket: UdpSocket::bind(listen_address, handle).context(ErrorKind::NetworkError)?,
        })
    }

    fn context_callback(cnx: *const picoquic_cnx_t,
                        stream_id: stream::Id, bytes: *const u8, length: size_t,
                        fin_or_event: picoquic_call_back_event_t, callback_ctx: *mut c_void) {
        
    }
}
