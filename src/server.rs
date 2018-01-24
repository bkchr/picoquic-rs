use error::*;
use stream;
use connection::{self, Connection};
use config::Config;
use ffi::QuicCtx;

use picoquic_sys::picoquic::{picoquic_call_back_event_t, picoquic_cnx_t};

use std::net::SocketAddr;
use std::os::raw::c_void;
use std::rc::Rc;
use std::mem;
use std::cell::RefCell;

use tokio_core::net::UdpSocket;
use tokio_core::reactor::Handle;

use futures::sync::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::{Future, Poll};
use futures::Async::NotReady;

use chrono::Utc;

fn get_timestamp() -> u64 {
    let now = Utc::now();

    now.timestamp() as u64 + now.timestamp_subsec_micros() as u64
}

struct ServerInner {
    socket: UdpSocket,
    context: Rc<RefCell<Context>>,
    quic: QuicCtx,
    /// Temporary buffer used for receiving and sending
    buffer: Vec<u8>,
}

impl ServerInner {
    pub fn new(
        listen_address: &SocketAddr,
        handle: &Handle,
        config: Config,
    ) -> Result<(ServerInner, UnboundedReceiver<Connection>), Error> {
        let (send, recv) = unbounded();
        let (context, c_ctx) = Context::new(send);

        let quic = QuicCtx::new(
            config,
            c_ctx,
            Some(new_connection_callback),
            get_timestamp(),
        )?;

        Ok((
            ServerInner {
                socket: UdpSocket::bind(listen_address, handle).context(ErrorKind::NetworkError)?,
                context,
                quic,
                buffer: vec![0; 1500],
            },
            recv,
        ))
    }

    /// Iterates over all connections for ready data and sends it.
    fn check_connections_for_packages_and_send(&mut self, current_time: u64) {
        let mut itr = self.quic.connection_iter();

        while let Some(con) = itr.next() {
            if con.is_disconnected() {
                con.delete();
                break;
            } else {
                if let Err(e) =
                    con.create_and_send_packet(&mut self.buffer, &mut self.socket, current_time)
                {
                    error!("error while sending connections packets: {:?}", e);
                }
            }
        }
    }
}

impl Future for ServerInner {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        // This can not return an error!
        assert!(self.context.borrow_mut().poll().is_ok());

        Ok(NotReady)
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

        let c_ctx = Rc::into_raw(ctx.clone()) as *mut c_void;

        assert_eq!(2, Rc::strong_count(&mut ctx));

        (ctx, c_ctx)
    }

    fn new_connection(&mut self, con: Connection, ctx: Rc<RefCell<connection::Context>>) {
        self.connections.push(ctx);
        if self.send_con.unbounded_send(con).is_err() {
            error!("error propagating new `Connection`, the receiving side probably closed!");
            //TODO: yeah we should end the `ServerInner` future here
        }
    }
}

impl Future for Context {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.connections.retain(|c| {
            c.borrow_mut()
                .poll()
                .map(|v| v.is_not_ready())
                .unwrap_or(false)
        });
        Ok(NotReady)
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
