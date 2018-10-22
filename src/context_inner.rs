use config::{Config, Role};
use connection::{self, Connection};
use error::*;
use ffi::QuicCtx;
use stream;

use picoquic_sys::picoquic::{
    picoquic_call_back_event_t, picoquic_cnx_t, PICOQUIC_MAX_PACKET_SIZE,
};

use std::cell::RefCell;
use std::io;
use std::mem;
use std::net::SocketAddr;
use std::os::raw::c_void;
use std::rc::Rc;
use std::time::{Duration, Instant};

use tokio_core::net::UdpSocket;
use tokio_core::reactor::{Handle, Timeout};

use futures::sync::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::sync::oneshot;
use futures::Async::{NotReady, Ready};
use futures::{task, Future, Poll, Stream};

type NewConnectionMsg = (
    SocketAddr,
    String,
    oneshot::Sender<Result<Connection, Error>>,
);

pub struct ContextInner {
    socket: UdpSocket,
    context: Rc<RefCell<CContext>>,
    quic: QuicCtx,
    /// Temporary buffer used for receiving and sending
    buffer: Vec<u8>,
    /// Picoquic requires to be woken up to handle resend,
    /// drop of connections(because of inactivity), etc..
    timer: Timeout,
    recv_connect: UnboundedReceiver<NewConnectionMsg>,
    /// The keep alive interval for client connections
    client_keep_alive_interval: Option<Duration>,
}

impl ContextInner {
    pub fn new(
        listen_address: &SocketAddr,
        handle: &Handle,
        config: Config,
    ) -> Result<
        (
            ContextInner,
            UnboundedReceiver<Connection>,
            NewConnectionHandle,
        ),
        Error,
    > {
        let (client_keep_alive_interval, server_keep_alive_interval) =
            match config.keep_alive_sender {
                Role::Client => (config.keep_alive_interval, None),
                Role::Server => (None, config.keep_alive_interval),
            };

        let (send, recv) = unbounded();
        let (context, c_ctx) = CContext::new(send, server_keep_alive_interval);

        let quic = QuicCtx::new(config, c_ctx, Some(new_connection_callback))?;

        let (send_connect, recv_connect) = unbounded();
        let connect = NewConnectionHandle { send: send_connect };

        Ok((
            ContextInner {
                socket: UdpSocket::bind(listen_address, handle).context(ErrorKind::NetworkError)?,
                context,
                quic,
                buffer: vec![0; PICOQUIC_MAX_PACKET_SIZE as usize],
                timer: Timeout::new(Duration::from_secs(10), handle).context(ErrorKind::Unknown)?,
                recv_connect,
                client_keep_alive_interval,
            },
            recv,
            connect,
        ))
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.socket.local_addr().unwrap()
    }

    /// Check if we should create a new connection
    fn check_for_new_connection_request(&mut self, current_time: u64) {
        loop {
            match self.recv_connect.poll() {
                Err(_) | Ok(NotReady) | Ok(Ready(None)) => break,
                Ok(Ready(Some((addr, server_name, sender)))) => {
                    let ctx = match Connection::new(
                        &self.quic,
                        addr,
                        self.local_addr(),
                        server_name,
                        current_time,
                        self.client_keep_alive_interval,
                        sender,
                    ) {
                        Ok(r) => r,
                        Err(e) => {
                            error!("could not create new connection: {:?}", e);
                            continue;
                        }
                    };

                    self.context.borrow_mut().connections.push(ctx);
                }
            }
        }
    }

    /// Iterates over all connections for data that is ready and sends it.
    fn send_connection_packets(&mut self, current_time: u64) {
        let itr = self.quic.connection_iter();

        for con in itr {
            if self.socket.poll_write().is_not_ready() {
                // The socket is not ready to send data
                break;
            }

            if con.is_disconnected() {
                con.delete();
                break;
            } else {
                match con.prepare_packet(&mut self.buffer, current_time) {
                    Ok(Some((len, addr))) => {
                        let _ = self.socket.send_to(&self.buffer[..len], &addr);
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
        let itr = self.quic.stateless_packet_iter();

        for packet in itr {
            if self.socket.poll_write().is_not_ready() {
                // The socket is not ready to send data
                break;
            }

            let _ = self
                .socket
                .send_to(packet.get_data(), &packet.get_peer_addr());
        }
    }

    /// Resets the timer to fire next at the given time.
    /// Returns true if polling the timer returned `NotReady`, otherwise false.
    fn reset_timer(&mut self, at: Instant) -> bool {
        self.timer.reset(at);
        // Poll the timer once, to register the current task to be woken up when the
        // timer finishes.
        self.timer.poll().map(|v| v.is_not_ready()).unwrap_or(false)
    }
}

impl Future for ContextInner {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        let mut loops_without_sleep = 0;
        // The maximum number of times, we are allowed to loop without returning from this future.
        // In some circumstances this loop runs forever(Picoquic always return `wake_time = 0`)
        // or the overlying application layer wants to send a lot of data. If we reach the maximum
        // loop count, we queue the current task to be woken up again and return `Ok(NotReady)`.
        let max_loops_without_sleep = 50;

        loop {
            let current_time = self.quic.get_current_time();

            self.check_for_new_connection_request(current_time);

            self.check_for_incoming_data(current_time);

            self.send_stateless_packets();

            // This checks all connection contexts if there is data that needs to be send
            assert!(self.context.borrow_mut().poll().is_ok());

            // All data that was send by the connection contexts, is collected to `Packet`'s per
            // connection and is send via the `UdpSocket`.
            self.send_connection_packets(current_time);

            let next_wake = self.quic.get_next_wake_up_time(current_time);

            if loops_without_sleep >= max_loops_without_sleep {
                task::current().notify();
                return Ok(NotReady);
            } else if let Some(next_wake) = next_wake {
                if !self.reset_timer(next_wake) {
                    // It can happen that the timer fires instantly, because the given `next_wake`
                    // time point wasn't far enough in the future or the system is under high load,
                    // etc.... So, we just execute the loop another time.
                    loops_without_sleep += 1;
                } else {
                    return Ok(NotReady);
                }
            } else {
                loops_without_sleep += 1;
            }
        }
    }
}

/// The callback context that is given as `ctx` argument to `new_connection_callback`.
struct CContext {
    connections: Vec<Rc<RefCell<connection::Context>>>,
    send_con: UnboundedSender<Connection>,
    server_keep_alive_interval: Option<Duration>,
}

impl CContext {
    fn new(
        send_con: UnboundedSender<Connection>,
        server_keep_alive_interval: Option<Duration>,
    ) -> (Rc<RefCell<CContext>>, *mut c_void) {
        let ctx = Rc::new(RefCell::new(CContext {
            connections: Vec::new(),
            send_con,
            server_keep_alive_interval,
        }));

        let c_ctx = Rc::into_raw(ctx.clone()) as *mut c_void;

        assert_eq!(2, Rc::strong_count(&ctx));

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

    let (con, con_ctx) = Connection::from_incoming(
        cnx,
        stream_id,
        bytes,
        length,
        event,
        ctx.borrow().server_keep_alive_interval,
    );

    ctx.borrow_mut().new_connection(con, con_ctx);

    mem::forget(ctx);
}

#[derive(Clone)]
pub struct NewConnectionHandle {
    send: UnboundedSender<NewConnectionMsg>,
}

impl NewConnectionHandle {
    /// Creates a new connection to the given server.
    ///
    /// addr - The address of the server.
    /// server_name - The name of the server that will be used by TLS to verify the certificate.
    pub fn new_connection<T: Into<String>>(
        &mut self,
        addr: SocketAddr,
        server_name: T,
    ) -> NewConnectionFuture {
        let (sender, recv) = oneshot::channel();

        let _ = self.send.unbounded_send((addr, server_name.into(), sender));

        NewConnectionFuture { recv }
    }
}

pub struct NewConnectionFuture {
    recv: oneshot::Receiver<Result<Connection, Error>>,
}

impl Future for NewConnectionFuture {
    type Item = Connection;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        try_ready!(self.recv.poll()).map(Ready)
    }
}
