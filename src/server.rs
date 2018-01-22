use error::*;
use stream;
use connection::{self, Connection};
use config::Config;

use picoquic_sys::picoquic::{picoquic_call_back_event_t, picoquic_cnx_t, picoquic_create,
                             picoquic_quic_t};

use std::net::SocketAddr;
use std::os::raw::c_void;
use std::rc::Rc;
use std::mem;
use std::cell::RefCell;
use std::ffi::CString;
use std::ptr;

use tokio_core::net::UdpSocket;
use tokio_core::reactor::Handle;

use futures::sync::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};

use chrono::Utc;

fn get_timestamp() -> u64 {
    let now = Utc::now();

    now.timestamp() as u64 + now.timestamp_subsec_micros() as u64
}

fn create_picoquic(ctx: *mut c_void, mut config: Config) -> Result<*mut picoquic_quic_t, Error> {
    // The number of buckets that picoquic will allocate for connections
    // The buckets itself are a linked list
    let connection_buckets = 16;

    let cert_filename = CString::new(config.cert_filename).context(ErrorKind::CStringError)?;
    let key_filename = CString::new(config.key_filename).context(ErrorKind::CStringError)?;
    let reset_seed = config
        .reset_seed
        .map(|mut v| v.as_mut_ptr())
        .unwrap_or_else(|| ptr::null_mut());

    Ok(unsafe {
        picoquic_create(
            connection_buckets,
            cert_filename.as_ptr(),
            ptr::null(),
            key_filename.as_ptr(),
            Some(new_connection_callback),
            ctx,
            None,
            ptr::null_mut(),
            reset_seed,
            get_timestamp(),
            ptr::null_mut(),
            ptr::null(),
            ptr::null(),
            0,
        )
    })
}

struct ServerInner {
    socket: UdpSocket,
    context: Rc<RefCell<Context>>,
    quic: *mut picoquic_quic_t,
}

impl ServerInner {
    pub fn new(
        listen_address: &SocketAddr,
        handle: &Handle,
        config: Config,
    ) -> Result<(ServerInner, UnboundedReceiver<Connection>), Error> {
        let (send, recv) = unbounded();
        let (context, c_ctx) = Context::new(send);

        let quic = create_picoquic(c_ctx, config)?;

        Ok((
            ServerInner {
                socket: UdpSocket::bind(listen_address, handle).context(ErrorKind::NetworkError)?,
                context,
                quic,
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

        let c_ctx = Rc::into_raw(ctx.clone()) as *mut c_void;

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
