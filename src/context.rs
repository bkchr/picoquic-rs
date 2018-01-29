use error::*;
use stream;
use connection::{self, Connection};
use config::Config;
use ffi::QuicCtx;

use picoquic_sys::picoquic::{picoquic_call_back_event_t, picoquic_cnx_t, PICOQUIC_MAX_PACKET_SIZE};

use std::net::SocketAddr;
use std::os::raw::c_void;
use std::rc::Rc;
use std::mem;
use std::cell::RefCell;
use std::io;
use std::time::Duration;

use tokio_core::net::UdpSocket;
use tokio_core::reactor::{Handle, Timeout};

use futures::sync::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::{Future, Poll, Stream};
use futures::Async::NotReady;

use chrono::Utc;

pub struct Context {
    recv_con: UnboundedReceiver<Connection>,
    local_addr: SocketAddr,
}

impl Context {
    pub fn new(
        listen_address: &SocketAddr,
        handle: &Handle,
        config: Config,
    ) -> Result<Context, Error> {
        let (inner, recv_con) = ContextInner::new(listen_address, handle, config)?;

        let local_addr = inner.local_addr();

        // start the inner future
        handle.spawn(inner);

        Ok(Context {
            recv_con,
            local_addr,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
}

impl Stream for Context {
    type Item = Connection;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.recv_con.poll().map_err(|_| ErrorKind::Unknown.into())
    }
}

fn get_timestamp() -> u64 {
    let now = Utc::now();

    now.timestamp() as u64 + now.timestamp_subsec_micros() as u64
}

struct ContextInner {
    socket: UdpSocket,
    context: Rc<RefCell<CContext>>,
    quic: QuicCtx,
    /// Temporary buffer used for receiving and sending
    buffer: Vec<u8>,
    /// Picoquic requires to be woken up to handle resend,
    /// drop of connections(because of inactivity), etc..
    timer: Timeout,
}

impl ContextInner {
    pub fn new(
        listen_address: &SocketAddr,
        handle: &Handle,
        config: Config,
    ) -> Result<(ContextInner, UnboundedReceiver<Connection>), Error> {
        let (send, recv) = unbounded();
        let (context, c_ctx) = CContext::new(send);

        let quic = QuicCtx::new(
            config,
            c_ctx,
            Some(new_connection_callback),
            get_timestamp(),
        )?;

        Ok((
            ContextInner {
                socket: UdpSocket::bind(listen_address, handle).context(ErrorKind::NetworkError)?,
                context,
                quic,
                buffer: vec![0; PICOQUIC_MAX_PACKET_SIZE as usize],
                timer: Timeout::new(Duration::from_secs(10), handle).context(ErrorKind::Unknown)?,
            },
            recv,
        ))
    }

    fn local_addr(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
    }

    /// Iterates over all connections for data that is ready and sends it.
    fn send_connection_packets(&mut self, current_time: u64) {
        let mut itr = self.quic.connection_iter();

        while let Some(con) = itr.next() {
            if self.socket.poll_write().is_not_ready() {
                // The socket is not ready to send data
                break;
            }

            if con.is_disconnected() {
                con.delete();
                break;
            } else {
                match con.create_and_prepare_packet(&mut self.buffer, current_time) {
                    Ok(Some(len)) => {
                        let _ = self.socket
                            .send_to(&self.buffer[..len], &con.get_peer_addr());
                    }
                    Ok(None) => {}
                    Err(e) => {
                        error!("error while sending connections packets: {:?}", e);
                    }
                };
            }
        }
    }

    /// Checks the `UdpSocket` for incoming data
    fn check_for_incoming_data(&mut self, current_time: u64) {
        fn wrapper(
            buf: &mut [u8],
            socket: &mut UdpSocket,
            quic: &mut QuicCtx,
            current_time: u64,
        ) -> Poll<Option<()>, io::Error> {
            loop {
                let (len, addr) = try_nb!(socket.recv_from(buf));
                quic.incoming_data(
                    &mut buf[..len],
                    socket.local_addr().unwrap(),
                    addr,
                    current_time,
                );
            }
        }

        let _ = wrapper(
            &mut self.buffer,
            &mut self.socket,
            &mut self.quic,
            current_time,
        );
    }

    fn send_stateless_packets(&mut self) {
        let mut itr = self.quic.stateless_packet_iter();

        while let Some(packet) = itr.next() {
            if self.socket.poll_write().is_not_ready() {
                // The socket is not ready to send data
                break;
            }

            let _ = self.socket
                .send_to(packet.get_data(), &packet.get_peer_addr());
        }
    }
}

impl Future for ContextInner {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            let current_time = get_timestamp();

            self.check_for_incoming_data(current_time);

            self.send_stateless_packets();

            // This checks all connection contexts if there is data that need to be send
            assert!(self.context.borrow_mut().poll().is_ok());

            // All data that was send by the connection contexts, is collected to `Packet`'s per
            // connection and is send via the `UdpSocket`.
            self.send_connection_packets(current_time);

            let next_wake = self.quic.get_next_wake_up_time(current_time);

            if let Some(next_wake) = next_wake {
                self.timer.reset(next_wake);
                // Poll the timer once, to register the current task to be woken up when the
                // timer finishes.
                assert!(self.timer.poll().map(|v| v.is_not_ready()).unwrap_or(false));

                return Ok(NotReady);
            }
        }
    }
}

/// The callback context that is given as `ctx` argument to `new_connection_callback`.
struct CContext {
    connections: Vec<Rc<RefCell<connection::Context>>>,
    send_con: UnboundedSender<Connection>,
}

impl CContext {
    fn new(send_con: UnboundedSender<Connection>) -> (Rc<RefCell<CContext>>, *mut c_void) {
        let mut ctx = Rc::new(RefCell::new(CContext {
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

impl Future for CContext {
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

fn get_context(ctx: *mut c_void) -> Rc<RefCell<CContext>> {
    unsafe { Rc::from_raw(ctx as *mut RefCell<CContext>) }
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
