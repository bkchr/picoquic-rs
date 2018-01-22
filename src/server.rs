use error::*;
use stream;
use connection::{self, Connection};

use picoquic_sys::picoquic::{picoquic_call_back_event_t, picoquic_cnx_t};

use std::net::SocketAddr;
use std::os::raw::c_void;
use std::rc::Rc;
use std::mem;
use std::cell::RefCell;

use tokio_core::net::UdpSocket;
use tokio_core::reactor::Handle;

use futures::sync::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};

struct ServerInner {
    socket: UdpSocket,
    context: Rc<RefCell<Context>>,
}

impl ServerInner {
    pub fn new(
        listen_address: &SocketAddr,
        handle: &Handle,
    ) -> Result<(ServerInner, UnboundedReceiver<Connection>), Error> {
        let (send, recv) = unbounded();
        let (ctx, c_ctx) = Context::new(send);

        Ok((
            ServerInner {
                socket: UdpSocket::bind(listen_address, handle).context(ErrorKind::NetworkError)?,
                context: ctx,
            },
            recv,
        ))
    }
}

struct Context {
    connections: Vec<Rc<RefCell<connection::Context>>>,
    send_con: UnboundedSender<Connection>,
}

impl Context {
    fn new(send_con: UnboundedSender<Connection>) -> (Rc<RefCell<Context>>, *mut c_void) {
        let mut ctx = Rc::new(RefCell::new(Context {
            connections: Vec::new(),
            send_con,
        }));

        let c_ctx = unsafe { Rc::into_raw(ctx.clone()) as *mut c_void };

        assert_eq!(2, Rc::strong_count(&mut ctx));

        (ctx, c_ctx)
    }

    fn new_connection(&mut self, con: Connection, ctx: Rc<RefCell<connection::Context>>) {
        self.connections.push(ctx);
        if self.send_con.unbounded_send(con).is_err() {
            error!("error propagating new `Connection`, the receiving side probably closed!");
            //TODO: yeah we should end here the `ServerInner` future
        }
    }
}

fn get_context(ctx: *mut c_void) -> Rc<RefCell<Context>> {
    unsafe { Rc::from_raw(ctx as *mut RefCell<Context>) }
}

unsafe extern "C" fn new_connection_callback(
    cnx: *mut picoquic_cnx_t,
    stream_id: stream::Id,
    bytes: *mut u8,
    length: usize,
    event: picoquic_call_back_event_t,
    ctx: *mut c_void,
) {
    assert!(!ctx.is_null());

    let ctx = get_context(ctx);

    let (con, con_ctx) = Connection::new(cnx, stream_id, bytes, length, event);

    ctx.borrow_mut().new_connection(con, con_ctx);

    mem::forget(ctx);
}
