use crate::config::{Config, Role};
use crate::connection::{self, Connection};
use crate::error::*;
use crate::ffi::{self, QuicCtx};
use crate::stream;

use picoquic_sys::picoquic::{
    picoquic_call_back_event_t, picoquic_cnx_t, PICOQUIC_MAX_PACKET_SIZE,
};

use std::{
    io, mem,
    net::SocketAddr,
    os::raw::c_void,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio_timer::Delay;
use tokio_udp::UdpSocket;

use futures::{
    sync::{
        mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
        oneshot,
    },
    task,
    Async::{NotReady, Ready},
    Future, Poll, Stream,
};

type NewConnectionMsg = (
    SocketAddr,
    String,
    oneshot::Sender<Result<Connection, Error>>,
);

pub struct ContextInner {
    socket: UdpSocket,
    context: Arc<Mutex<CContext>>,
    quic: QuicCtx,
    /// Temporary buffer used for receiving and sending
    buffer: Vec<u8>,
    /// Data in the buffer that was not send, because the socket was full.
    not_send_length: Option<(usize, SocketAddr)>,
    /// Picoquic requires to be woken up to handle resend,
    /// drop of connections(because of inactivity), etc..
    timer: Delay,
    recv_connect: UnboundedReceiver<NewConnectionMsg>,
    /// The keep alive interval for client connections
    client_keep_alive_interval: Option<Duration>,
    close_handle: Option<oneshot::Sender<()>>,
    stream_send_channel_default_size: usize,
}

impl ContextInner {
    pub fn new(
        listen_address: &SocketAddr,
        config: Config,
    ) -> Result<
        (
            ContextInner,
            UnboundedReceiver<Connection>,
            NewConnectionHandle,
            oneshot::Receiver<()>,
        ),
        Error,
    > {
        let stream_send_channel_default_size = config.stream_send_channel_default_size;
        let (client_keep_alive_interval, server_keep_alive_interval) =
            match config.keep_alive_sender {
                Role::Client => (config.keep_alive_interval, None),
                Role::Server => (None, config.keep_alive_interval),
            };

        let (send, recv) = unbounded();
        let (context, c_ctx) = CContext::new(
            send,
            server_keep_alive_interval,
            stream_send_channel_default_size,
        );

        let quic = QuicCtx::new(config, c_ctx, Some(new_connection_callback))?;

        let (send_connect, recv_connect) = unbounded();
        let connect = NewConnectionHandle { send: send_connect };
        let (close_handle_send, close_handle) = oneshot::channel();

        Ok((
            ContextInner {
                socket: UdpSocket::bind(listen_address).context(ErrorKind::NetworkError)?,
                context,
                quic,
                buffer: vec![0; PICOQUIC_MAX_PACKET_SIZE as usize],
                timer: Delay::new(Instant::now() + Duration::from_secs(10)),
                recv_connect,
                client_keep_alive_interval,
                close_handle: Some(close_handle_send),
                not_send_length: None,
                stream_send_channel_default_size,
            },
            recv,
            connect,
            close_handle,
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
                        self.stream_send_channel_default_size,
                    ) {
                        Ok(r) => r,
                        Err(e) => {
                            error!("could not create new connection: {:?}", e);
                            continue;
                        }
                    };

                    self.context.lock().unwrap().connections.push(ctx);
                }
            }
        }
    }

    /// Send the data in `self.buffer`.
    /// Returns true if the data could be send.
    fn send_data_in_buffer(&mut self, len: usize, addr: SocketAddr) -> bool {
        match self.socket.poll_send_to(&self.buffer[..len], &addr) {
            Ok(NotReady) => {
                self.not_send_length = Some((len, addr));
                trace!("Socket is full and {} bytes was not send, yet!", len);
                false
            }
            _ => true,
        }
    }

    /// Iterates over all connections for data that is ready and sends it.
    fn send_connection_packets(&mut self, current_time: u64) {
        if let Some((len, addr)) = self.not_send_length.take() {
            if !self.send_data_in_buffer(len, addr) {
                return;
            }
        }

        let itr = self.quic.ordered_connection_iter(current_time);

        for con in itr {
            if con.is_disconnected() {
                con.delete();
                break;
            } else {
                match con.prepare_packet(&mut self.buffer, current_time) {
                    Ok(Some((len, addr))) => {
                        if !self.send_data_in_buffer(len, addr) {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        debug!("error while sending connections packets: {:?}", e);
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
                let (len, addr) = try_ready!(socket.poll_recv_from(buf));
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

    fn send_stateless_packets(&mut self) -> Poll<(), Error> {
        let itr = self.quic.stateless_packet_iter();

        for packet in itr {
            try_ready!(self
                .socket
                .poll_send_to(packet.get_data(), &packet.get_peer_addr()));
        }

        Ok(Ready(()))
    }

    /// Resets the timer to fire next at the given time.
    /// Returns true if polling the timer returned `NotReady`, otherwise false.
    fn reset_timer(&mut self, at: Instant) -> bool {
        self.timer.reset(at);
        // Poll the timer once, to register the current task to be woken up when the
        // timer finishes.
        self.timer.poll().map(|v| v.is_not_ready()).unwrap_or(false)
    }

    fn is_context_dropped(&mut self) -> bool {
        self.close_handle
            .as_mut()
            .map(|h| match h.poll_cancel() {
                Ok(Ready(_)) | Err(_) => true,
                Ok(NotReady) => false,
            })
            .unwrap_or(true)
    }
}

impl Drop for ContextInner {
    fn drop(&mut self) {
        self.close_handle.take().map(|h| h.send(()));
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
            // This checks all connection contexts if there is data that needs to be send.
            // We do this before acquiring the current time, to make sure that we send the data
            // in this loop (if permitted).
            assert!(self.context.lock().unwrap().poll().is_ok());

            let current_time = self.quic.get_current_time();

            if self.is_context_dropped() {
                trace!("`Context` dropped, will end `ContextInner`.");
                return Ok(Ready(()));
            }

            self.check_for_new_connection_request(current_time);

            self.check_for_incoming_data(current_time);

            let _ = self.send_stateless_packets();

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
    connections: Vec<Arc<Mutex<connection::Context>>>,
    send_con: UnboundedSender<Connection>,
    server_keep_alive_interval: Option<Duration>,
    stream_send_channel_default_size: usize,
}

impl CContext {
    fn new(
        send_con: UnboundedSender<Connection>,
        server_keep_alive_interval: Option<Duration>,
        stream_send_channel_default_size: usize,
    ) -> (Arc<Mutex<CContext>>, *mut c_void) {
        let ctx = Arc::new(Mutex::new(CContext {
            connections: Vec::new(),
            send_con,
            server_keep_alive_interval,
            stream_send_channel_default_size,
        }));

        let c_ctx = Arc::into_raw(ctx.clone()) as *mut c_void;

        assert_eq!(2, Arc::strong_count(&ctx));

        (ctx, c_ctx)
    }

    fn new_connection(&mut self, con: Connection, ctx: Arc<Mutex<connection::Context>>) {
        self.connections.push(ctx);
        let _ = self.send_con.unbounded_send(con);
    }
}

impl Future for CContext {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.connections.retain(|c| {
            c.lock()
                .unwrap()
                .poll()
                .map(|v| v.is_not_ready())
                .unwrap_or(false)
        });
        Ok(NotReady)
    }
}

fn get_context(ctx: *mut c_void) -> Arc<Mutex<CContext>> {
    unsafe { Arc::from_raw(ctx as *mut Mutex<CContext>) }
}

unsafe extern "C" fn new_connection_callback(
    cnx: *mut picoquic_cnx_t,
    stream_id: stream::Id,
    bytes: *mut u8,
    length: usize,
    event: picoquic_call_back_event_t,
    ctx: *mut c_void,
    _: *mut c_void,
) -> i32 {
    assert!(!ctx.is_null());

    // early out, if the connection is already going to be closed, we don't need to handle it.
    if ffi::Connection::from(cnx).is_going_to_close() {
        return 0;
    }

    let ctx = get_context(ctx);
    {
        let mut ctx_locked = ctx.lock().unwrap();

        let (con, con_ctx) = Connection::from_incoming(
            cnx,
            stream_id,
            bytes,
            length,
            event,
            ctx_locked.server_keep_alive_interval,
            ctx_locked.stream_send_channel_default_size,
        );

        ctx_locked.new_connection(con, con_ctx);
    }

    mem::forget(ctx);
    0
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
