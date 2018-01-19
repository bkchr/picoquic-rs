use error::*;
use stream;
use connection::Connection;

use picoquic_sys::picoquic::{self, picoquic_call_back_event_t, picoquic_cnx_t,
                             picoquic_set_callback};

use std::net::SocketAddr;
use std::os::raw::c_void;
use std::collections::HashMap;
use std::rc::Rc;
use std::mem;
use std::slice;

use tokio_core::net::UdpSocket;
use tokio_core::reactor::Handle;

struct ServerInner {}

pub struct Server {
    socket: UdpSocket,
    inner: Rc<ServerInner>,
}

impl Server {
    pub fn new(listen_address: &SocketAddr, handle: &Handle) -> Result<Server, Error> {
        Ok(Server {
            socket: UdpSocket::bind(listen_address, handle).context(ErrorKind::NetworkError)?,
            inner: Rc::new(ServerInner {}),
        })
    }
}

type Context = Box<(Rc<ServerInner>, Option<Connection>)>;

unsafe extern "C" fn new_connection_callback(
    cnx: *mut picoquic_cnx_t,
    stream_id: stream::Id,
    bytes: *mut u8,
    length: usize,
    event: picoquic_call_back_event_t,
    ctx: *mut c_void,
) {
    assert!(!ctx.is_null());

    if event == picoquic::picoquic_call_back_event_t_picoquic_callback_close
        || event == picoquic::picoquic_call_back_event_t_picoquic_callback_application_close
    {
        free_context(ctx);
    } else {
        let ctx = get_context(cnx, ctx);
        {
            let con = ctx.1.as_ref().unwrap();

            let data = slice::from_raw_parts(bytes, length as usize);
            con.recv_data(stream_id, data);
        }

        // the context must not be deleted!
        mem::forget(ctx);
    }
}
