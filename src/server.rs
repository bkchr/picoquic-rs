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

fn get_context_from_c_void(ctx: *mut c_void) -> Context {
    unsafe { Box::from_raw(ctx as *mut (Rc<ServerInner>, Option<Connection>)) }
}

fn get_context(cnx: *mut picoquic_cnx_t, ctx: *mut c_void) -> Context {
    let mut ctx = get_context_from_c_void(ctx);

    if ctx.1.is_none() {
        let inner = ctx.0.clone();
        mem::forget(ctx);

        let con = Some(Connection::new());
        ctx = unsafe {
            let c_ctx = Box::into_raw(Box::new((inner, con)));
            picoquic_set_callback(cnx, Some(recv_data_callback), c_ctx as *mut c_void);
            Box::from_raw(c_ctx)
        };
    }

    ctx
}

fn free_context(ctx: *mut c_void) {
    let ctx = get_context_from_c_void(ctx);

    if ctx.1.is_none() {
        // if the connection is `None`, we still got the initial context and that must not be
        // dropped in this function!
        mem::forget(ctx);
    }
}

unsafe extern "C" fn recv_data_callback(
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
