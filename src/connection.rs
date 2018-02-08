use error::*;
use stream::{self, Stream};
use ffi::{self, QuicCtx};

use picoquic_sys::picoquic::{self, picoquic_call_back_event_t, picoquic_cnx_t,
                             picoquic_set_callback};

use futures::sync::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::sync::oneshot;
use futures::{self, Future, Poll, Stream as FStream};
use futures::Async::{NotReady, Ready};

use std::collections::HashMap;
use std::collections::hash_map::Entry::{Occupied, Vacant};
use std::os::raw::c_void;
use std::mem;
use std::rc::Rc;
use std::cell::RefCell;
use std::slice;
use std::net::SocketAddr;

#[derive(Debug)]
enum Message {
    NewStream(Stream),
    Close,
}

/// Represents a connection to a peer.
pub struct Connection {
    msg_recv: UnboundedReceiver<Message>,
    peer_addr: SocketAddr,
    local_addr: SocketAddr,
    new_stream_handle: NewStreamHandle,
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
}

impl futures::Stream for Connection {
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
    ) -> (Connection, Rc<RefCell<Context>>) {
        let cnx = ffi::Connection::from(cnx);

        let (con, ctx, c_ctx) = Self::create(cnx, cnx.peer_addr(), cnx.local_addr(), false);

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
        current_time: u64,
    ) -> Result<(Connection, Rc<RefCell<Context>>), Error> {
        let cnx = ffi::Connection::new(quic, peer_addr, current_time)?;

        let (con, ctx, _) = Self::create(cnx, peer_addr, local_addr, true);

        Ok((con, ctx))
    }

    fn create(
        cnx: ffi::Connection,
        peer_addr: SocketAddr,
        local_addr: SocketAddr,
        is_client: bool,
    ) -> (Connection, Rc<RefCell<Context>>, *mut c_void) {
        let (sender, msg_recv) = unbounded();

        let (ctx, c_ctx, new_stream_handle) = Context::new(cnx, sender, is_client);

        let con = Connection {
            msg_recv,
            peer_addr,
            local_addr,
            new_stream_handle,
        };

        (con, ctx, c_ctx)
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
}

pub(crate) struct Context {
    send_msg: UnboundedSender<Message>,
    recv_create_stream: UnboundedReceiver<(stream::Type, oneshot::Sender<Stream>)>,
    streams: HashMap<stream::Id, stream::Context>,
    cnx: ffi::Connection,
    closed: bool,
    /// Is the connection initiated by us?
    is_client: bool,
    next_stream_id: u64,
    /// Stores requested `Streams` until the connection is ready to process data. (state == ready)
    wait_for_ready_state: Option<Vec<(Stream, oneshot::Sender<Stream>)>>,
}

impl Context {
    fn new(
        cnx: ffi::Connection,
        send_msg: UnboundedSender<Message>,
        is_client: bool,
    ) -> (Rc<RefCell<Context>>, *mut c_void, NewStreamHandle) {
        let (send_create_stream, recv_create_stream) = unbounded();

        let new_stream_handle = NewStreamHandle {
            send: send_create_stream,
        };

        let mut ctx = Rc::new(RefCell::new(Context {
            send_msg,
            streams: Default::default(),
            cnx,
            closed: false,
            recv_create_stream,
            is_client,
            next_stream_id: 0,
            wait_for_ready_state: Some(Vec::new()),
        }));

        // Convert the `Context` to a `*mut c_void` and reset the callback to the
        // `recv_data_callback`
        let c_ctx = unsafe {
            let c_ctx = Rc::into_raw(ctx.clone()) as *mut c_void;
            picoquic_set_callback(cnx.as_ptr(), Some(recv_data_callback), c_ctx);
            c_ctx
        };

        // The reference counter needs to be 2 at this point
        assert_eq!(2, Rc::strong_count(&mut ctx));

        (ctx, c_ctx, new_stream_handle)
    }

    fn recv_data(&mut self, id: stream::Id, data: &[u8], event: picoquic_call_back_event_t) {
        let new_stream_handle = match self.streams.entry(id) {
            Occupied(mut entry) => {
                entry.get_mut().recv_data(data, event);
                None
            }
            Vacant(entry) => {
                let (stream, mut ctx) = Stream::new(id, self.cnx, self.is_client);

                ctx.recv_data(data, event);
                entry.insert(ctx);
                Some(stream)
            }
        };

        if let Some(stream) = new_stream_handle {
            if self.send_msg
                .unbounded_send(Message::NewStream(stream))
                .is_err()
            {
                info!("will close connection, because `Connection` instance was dropped.");
                self.close();
            }
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

                    let (stream, ctx) = Stream::new(id, self.cnx, self.is_client);
                    assert!(self.streams.insert(id, ctx).is_none());

                    match self.wait_for_ready_state {
                        Some(ref mut wait) => wait.push((stream, sender)),
                        None => {
                            let _ = sender.send(stream);
                        }
                    };
                }
            }
        }
    }

    fn close(&mut self) {
        self.cnx.close();
        self.closed = true;
        let _ = self.send_msg.unbounded_send(Message::Close);
    }

    fn process_wait_for_ready_state(&mut self) {
        match self.wait_for_ready_state.take() {
            Some(wait) => wait.into_iter().for_each(|(stream, sender)| {
                sender.send(stream).unwrap();
            }),
            None => panic!("connection can only switches once into `ready` state!"),
        };
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
    send: UnboundedSender<(stream::Type, oneshot::Sender<Stream>)>,
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
    recv: oneshot::Receiver<Stream>,
}

impl Future for NewStreamFuture {
    type Item = Stream;
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.recv.poll().map_err(|_| ErrorKind::Unknown.into())
    }
}
