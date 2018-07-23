use error::*;
use ffi::{self, QuicCtx};
use stream::{self, Stream};

use picoquic_sys::picoquic::{
    self, picoquic_call_back_event_t, picoquic_cnx_t, picoquic_set_callback,
};

use futures::sync::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::sync::oneshot;
use futures::Async::{NotReady, Ready};
use futures::{Future, Poll, Stream as FStream};

use std::cell::RefCell;
use std::collections::hash_map::Entry::{Occupied, Vacant};
use std::collections::HashMap;
use std::mem;
use std::net::SocketAddr;
use std::os::raw::c_void;
use std::rc::Rc;
use std::slice;
use std::time::Duration;

pub type Id = u64;

#[derive(Debug)]
enum Message {
    NewStream(Stream),
    Close,
    Error(Error),
}

/// A `Connection` can either be `Incoming` or `Outgoing`.
#[derive(Debug, PartialEq, Copy, Clone)]
pub enum Type {
    /// The `Connection` was created, because a remote peer created it.
    Incoming,
    /// The `Connection` was created, because the local peer created it.
    Outgoing,
}

struct ConnectionBuilder {
    msg_recv: UnboundedReceiver<Message>,
    close_send: oneshot::Sender<()>,
    peer_addr: SocketAddr,
    local_addr: SocketAddr,
    new_stream_handle: NewStreamHandle,
    ctype: Type,
}

impl ConnectionBuilder {
    fn new(
        msg_recv: UnboundedReceiver<Message>,
        close_send: oneshot::Sender<()>,
        peer_addr: SocketAddr,
        local_addr: SocketAddr,
        new_stream_handle: NewStreamHandle,
        ctype: Type,
    ) -> ConnectionBuilder {
        ConnectionBuilder {
            msg_recv,
            close_send,
            peer_addr,
            local_addr,
            new_stream_handle,
            ctype,
        }
    }

    fn build(self, id: Id) -> Connection {
        Connection {
            msg_recv: self.msg_recv,
            close_send: Some(self.close_send),
            peer_addr: self.peer_addr,
            local_addr: self.local_addr,
            new_stream_handle: self.new_stream_handle,
            ctype: self.ctype,
            id,
        }
    }
}

/// Represents a connection to a peer.
pub struct Connection {
    msg_recv: UnboundedReceiver<Message>,
    close_send: Option<oneshot::Sender<()>>,
    peer_addr: SocketAddr,
    local_addr: SocketAddr,
    new_stream_handle: NewStreamHandle,
    id: Id,
    ctype: Type,
}

impl Connection {
    /// Returns the address of the peer, this `Connection` is connected to.
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// Returns the address of the local `Context`, where it is listening on.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Returns the id of this `Connection`.
    /// The id is at the server and at the client the same.
    pub fn id(&self) -> Id {
        self.id
    }

    /// Returns the `Type` of this `Connection`.
    pub fn get_type(&self) -> Type {
        self.ctype
    }
}

impl FStream for Connection {
    type Item = Stream;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        match try_ready!(
            self.msg_recv
                .poll()
                .map_err(|_| Error::from(ErrorKind::Unknown))
        ) {
            Some(Message::Close) | None => Ok(Ready(None)),
            Some(Message::NewStream(s)) => Ok(Ready(Some(s))),
            Some(Message::Error(e)) => Err(e),
        }
    }
}

impl Connection {
    /// Creates a new `Connection` from an incoming connection.
    pub(crate) fn from_incoming(
        cnx: *mut picoquic_cnx_t,
        stream_id: stream::Id,
        data: *mut u8,
        len: usize,
        event: picoquic_call_back_event_t,
        keep_alive_interval: Option<Duration>,
    ) -> (Connection, Rc<RefCell<Context>>) {
        let cnx = ffi::Connection::from(cnx);

        let (builder, ctx, c_ctx) = Self::create_builder(
            cnx,
            cnx.peer_addr(),
            cnx.local_addr(),
            false,
            keep_alive_interval,
        );

        let con = builder.build(cnx.local_id());

        // Now we need to call the callback once manually to process the received data
        unsafe {
            recv_data_callback(cnx.as_ptr(), stream_id, data, len, event, c_ctx);
        }

        (con, ctx)
    }

    /// Creates a new `Connection` to the given `peer_addr` server.
    pub(crate) fn new(
        quic: &QuicCtx,
        peer_addr: SocketAddr,
        local_addr: SocketAddr,
        server_name: String,
        current_time: u64,
        keep_alive_interval: Option<Duration>,
        created_sender: oneshot::Sender<Result<Connection, Error>>,
    ) -> Result<(Rc<RefCell<Context>>), Error> {
        let cnx = ffi::Connection::new(quic, peer_addr, current_time, server_name)?;

        let (builder, ctx, _) =
            Self::create_builder(cnx, peer_addr, local_addr, true, keep_alive_interval);

        // set the builder and the sender as waiting for ready state payload
        ctx.borrow_mut()
            .set_wait_for_ready_state(builder, created_sender);

        Ok(ctx)
    }

    fn create_builder(
        cnx: ffi::Connection,
        peer_addr: SocketAddr,
        local_addr: SocketAddr,
        is_client: bool,
        keep_alive_interval: Option<Duration>,
    ) -> (ConnectionBuilder, Rc<RefCell<Context>>, *mut c_void) {
        let (sender, msg_recv) = unbounded();
        let (close_send, close_recv) = oneshot::channel();

        let (ctx, c_ctx, new_stream_handle) =
            Context::new(cnx, sender, close_recv, is_client, local_addr);

        if let Some(interval) = keep_alive_interval {
            cnx.enable_keep_alive(interval);
        }

        let builder = ConnectionBuilder::new(
            msg_recv,
            close_send,
            peer_addr,
            local_addr,
            new_stream_handle,
            cnx.con_type(),
        );

        (builder, ctx, c_ctx)
    }

    /// Creates a new bidirectional `Stream`.
    pub fn new_bidirectional_stream(&mut self) -> NewStreamFuture {
        self.new_stream_handle.new_bidirectional_stream()
    }

    /// Creates a new unidirectional `Stream`.
    pub fn new_unidirectional_stream(&mut self) -> NewStreamFuture {
        self.new_stream_handle.new_unidirectional_stream()
    }

    /// Returns a handle to create new `Stream`s for this connection.
    pub fn get_new_stream_handle(&self) -> NewStreamHandle {
        self.new_stream_handle.clone()
    }

    /// Immediately closes this connection.
    /// Any buffered data will be discarded.
    /// This function should only be used, if the application layer negotiated a close of the
    /// connection.
    pub fn close_immediately(mut self) {
        self.close_send.take().map(|s| s.send(()));
    }
}

pub(crate) struct Context {
    send_msg: UnboundedSender<Message>,
    close_recv: oneshot::Receiver<()>,
    recv_create_stream: UnboundedReceiver<(stream::Type, oneshot::Sender<Result<Stream, Error>>)>,
    streams: HashMap<stream::Id, stream::Context>,
    cnx: ffi::Connection,
    closed: bool,
    /// Is the connection initiated by us?
    is_client: bool,
    next_stream_id: u64,
    /// If we create an outgoing connection, we postpone the `Connection` creation to the point
    /// where the connection state is ready. This is necessary, because some information that we
    /// require for the `Connection` object is not available up to this point.
    wait_for_ready_state: Option<(
        ConnectionBuilder,
        oneshot::Sender<Result<Connection, Error>>,
    )>,
    local_addr: SocketAddr,
}

impl Context {
    fn new(
        cnx: ffi::Connection,
        send_msg: UnboundedSender<Message>,
        close_recv: oneshot::Receiver<()>,
        is_client: bool,
        local_addr: SocketAddr,
    ) -> (Rc<RefCell<Context>>, *mut c_void, NewStreamHandle) {
        let (send_create_stream, recv_create_stream) = unbounded();

        let new_stream_handle = NewStreamHandle {
            send: send_create_stream,
        };

        let ctx = Rc::new(RefCell::new(Context {
            send_msg,
            streams: Default::default(),
            cnx,
            closed: false,
            recv_create_stream,
            is_client,
            next_stream_id: 0,
            wait_for_ready_state: None,
            local_addr,
            close_recv,
        }));

        // Convert the `Context` to a `*mut c_void` and reset the callback to the
        // `recv_data_callback`
        let c_ctx = unsafe {
            let c_ctx = Rc::into_raw(ctx.clone()) as *mut c_void;
            picoquic_set_callback(cnx.as_ptr(), Some(recv_data_callback), c_ctx);
            c_ctx
        };

        // The reference counter needs to be 2 at this point
        assert_eq!(2, Rc::strong_count(&ctx));

        (ctx, c_ctx, new_stream_handle)
    }

    fn recv_data(&mut self, id: stream::Id, data: &[u8], event: picoquic_call_back_event_t) {
        let new_stream_handle = match self.streams.entry(id) {
            Occupied(mut entry) => {
                entry.get_mut().recv_data(data, event);
                None
            }
            Vacant(entry) => {
                let (stream, mut ctx) = Stream::new(id, self.cnx, self.local_addr, self.is_client);

                ctx.recv_data(data, event);
                entry.insert(ctx);
                Some(stream)
            }
        };

        if let Some(stream) = new_stream_handle {
            let _ = self.send_msg.unbounded_send(Message::NewStream(stream));
        }
    }

    /// Check for new streams to create and create these requested streams.
    fn check_create_stream_requests(&mut self) {
        loop {
            match self.recv_create_stream.poll() {
                Ok(Ready(None)) | Ok(NotReady) | Err(_) => break,
                Ok(Ready(Some((stype, sender)))) => {
                    let id = ffi::Connection::generate_stream_id(
                        self.next_stream_id,
                        self.is_client,
                        stype,
                    );
                    self.next_stream_id += 1;

                    let (stream, ctx) = Stream::new(id, self.cnx, self.local_addr, self.is_client);
                    assert!(self.streams.insert(id, ctx).is_none());

                    let _ = sender.send(Ok(stream));
                }
            }
        }
    }

    fn close(&mut self) {
        self.cnx.close();
        self.closed = true;
        self.streams
            .values_mut()
            .for_each(|s| s.handle_connection_close());
        let _ = self.send_msg.unbounded_send(Message::Close);
    }

    fn process_wait_for_ready_state(&mut self) {
        match self.wait_for_ready_state.take() {
            Some((builder, sender)) => {
                let id = self.cnx.local_id();
                let con = builder.build(id);

                let _ = sender.send(Ok(con));
            }
            None => panic!("connection can only switches once into `ready` state!"),
        };
    }

    fn set_wait_for_ready_state(
        &mut self,
        builder: ConnectionBuilder,
        sender: oneshot::Sender<Result<Connection, Error>>,
    ) {
        self.wait_for_ready_state = Some((builder, sender));
    }

    /// Checks if the connection had an error and handles it.
    fn check_and_handle_error(&mut self) {
        if let Some(err) = self.cnx.error() {
            self.streams
                .values_mut()
                .for_each(|s| s.handle_connection_error(err()));

            while let Ok(Ready(Some((_, sender)))) = self.recv_create_stream.poll() {
                let _ = sender.send(Err(err()));
            }

            match self.wait_for_ready_state.take() {
                Some((_, send)) => {
                    let _ = send.send(Err(err()));
                }
                None => {
                    let _ = self.send_msg.unbounded_send(Message::Error(err()));
                }
            }
        }
    }
}

impl Future for Context {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if self.closed {
            return Ok(Ready(()));
        }

        if self.wait_for_ready_state.is_some() && self.cnx.is_ready() {
            self.process_wait_for_ready_state();
        }

        self.streams
            .retain(|_, s| s.poll().map(|r| r.is_not_ready()).unwrap_or(false));

        self.check_create_stream_requests();

        // Check if the connection should be closed
        if let Ok(Ready(_)) = self.close_recv.poll() {
            self.close();
        }

        Ok(NotReady)
    }
}

fn get_context(ctx: *mut c_void) -> Rc<RefCell<Context>> {
    unsafe { Rc::from_raw(ctx as *mut RefCell<Context>) }
}

unsafe extern "C" fn recv_data_callback(
    _: *mut picoquic_cnx_t,
    stream_id: stream::Id,
    bytes: *mut u8,
    length: usize,
    event: picoquic_call_back_event_t,
    ctx: *mut c_void,
) {
    assert!(!ctx.is_null());
    let ctx = get_context(ctx);

    if event == picoquic::picoquic_call_back_event_t_picoquic_callback_close
        || event == picoquic::picoquic_call_back_event_t_picoquic_callback_application_close
    {
        // when Rc goes out of scope, it will dereference the Context pointer automatically
        ctx.borrow_mut().check_and_handle_error();
        ctx.borrow_mut().close();
    } else {
        let data = slice::from_raw_parts(bytes, length as usize);

        ctx.borrow_mut().recv_data(stream_id, data, event);

        // the context must not be dereferenced!
        mem::forget(ctx);
    }
}

/// A handle to create new `Stream`s for a connection.
#[derive(Clone)]
pub struct NewStreamHandle {
    send: UnboundedSender<(stream::Type, oneshot::Sender<Result<Stream, Error>>)>,
}

impl NewStreamHandle {
    /// Creates a new bidirectional `Stream`.
    pub fn new_bidirectional_stream(&mut self) -> NewStreamFuture {
        self.new_stream_handle(stream::Type::Bidirectional)
    }

    /// Creates a new unidirectional `Stream`.
    pub fn new_unidirectional_stream(&mut self) -> NewStreamFuture {
        self.new_stream_handle(stream::Type::Unidirectional)
    }

    fn new_stream_handle(&mut self, stype: stream::Type) -> NewStreamFuture {
        let (send, recv) = oneshot::channel();

        let _ = self.send.unbounded_send((stype, send));

        NewStreamFuture { recv }
    }
}

/// A future that resolves to a `Stream`.
/// This future is created by the `NewStreamHandle`.
pub struct NewStreamFuture {
    recv: oneshot::Receiver<Result<Stream, Error>>,
}

impl Future for NewStreamFuture {
    type Item = Stream;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.recv
            .poll()
            .map_err(|_| ErrorKind::Unknown.into())
            .and_then(|r| match r {
                Ready(v) => v.map(Ready),
                NotReady => Ok(NotReady),
            })
    }
}
