use error::*;
use stream::{self, Stream};
use ffi::{self, QuicCtx};

use picoquic_sys::picoquic::{self, picoquic_call_back_event_t, picoquic_close, picoquic_cnx_t,
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

pub enum Message {
    NewStream(Stream),
    Close,
}

pub struct Connection {
    msg_recv: UnboundedReceiver<Message>,
    peer_addr: SocketAddr,
    new_stream: NewStream,
}

impl Connection {
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }
}

impl futures::Stream for Connection {
    type Item = Message;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.msg_recv.poll().map_err(|_| ErrorKind::Unknown.into())
    }
}

impl Connection {
    pub(crate) fn from_incoming(
        cnx: *mut picoquic_cnx_t,
        stream_id: stream::Id,
        data: *mut u8,
        len: usize,
        event: picoquic_call_back_event_t,
    ) -> (Connection, Rc<RefCell<Context>>) {
        let (sender, msg_recv) = unbounded();

        let peer_addr = ffi::Connection::from(cnx).get_peer_addr();

        let (ctx, c_ctx, new_stream) = Context::new(cnx, sender, false);

        let con = Connection {
            msg_recv,
            peer_addr,
            new_stream,
        };

        // Now we need to call the callback once manually to process the received data
        unsafe {
            recv_data_callback(cnx, stream_id, data, len, event, c_ctx);
        }

        (con, ctx)
    }

    pub(crate) fn new(
        quic: &QuicCtx,
        peer_addr: SocketAddr,
        current_time: u64,
    ) -> Result<(Connection, Rc<RefCell<Context>>), Error> {
        let cnx = ffi::Connection::new(quic, peer_addr, current_time);

        if cnx.is_null() {
            return Err(ErrorKind::Unknown.into());
        }

        let (sender, msg_recv) = unbounded();

        let (ctx, _, new_stream) = Context::new(cnx, sender, true);

        let con = Connection {
            msg_recv,
            peer_addr,
            new_stream,
        };

        Ok((con, ctx))
    }

    pub fn new_bidirectional_stream(&mut self) -> NewStreamFuture {
        self.new_stream.new_unidirectional_stream()
    }

    pub fn new_unidirectional_stream(&mut self) -> NewStreamFuture {
        self.new_stream.new_bidirectional_stream()
    }

    pub fn get_new_stream(&self) -> NewStream {
        self.new_stream.clone()
    }
}

pub(crate) struct Context {
    send_msg: UnboundedSender<Message>,
    recv_create_stream: UnboundedReceiver<(stream::Type, oneshot::Sender<Stream>)>,
    streams: HashMap<stream::Id, stream::Context>,
    cnx: *mut picoquic_cnx_t,
    closed: bool,
    /// Is the connection initiated by us?
    is_client: bool,
    next_stream_id: u64,
}

impl Context {
    fn new(
        cnx: *mut picoquic_cnx_t,
        send_msg: UnboundedSender<Message>,
        is_client: bool,
    ) -> (Rc<RefCell<Context>>, *mut c_void, NewStream) {
        let (send_create_stream, recv_create_stream) = unbounded();

        let new_stream = NewStream {
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
        }));

        // Convert the `Context` to a `*mut c_void` and reset the callback to the
        // `recv_data_callback`
        let c_ctx = unsafe {
            let c_ctx = Rc::into_raw(ctx.clone()) as *mut c_void;
            picoquic_set_callback(cnx, Some(recv_data_callback), c_ctx);
            c_ctx
        };

        // The reference counter needs to be 2 at this point
        assert_eq!(2, Rc::strong_count(&mut ctx));

        (ctx, c_ctx, new_stream)
    }

    fn recv_data(&mut self, id: stream::Id, data: &[u8], event: picoquic_call_back_event_t) {
        let new_stream = match self.streams.entry(id) {
            Occupied(mut entry) => {
                entry.get_mut().recv_data(data, event);
                None
            }
            Vacant(entry) => {
                let (stream, mut ctx) = Stream::new(id, self.cnx);

                ctx.recv_data(data, event);
                entry.insert(ctx);
                Some(stream)
            }
        };

        if let Some(stream) = new_stream {
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
                    let id =
                        ffi::Connection::get_stream_id(self.next_stream_id, self.is_client, stype);
                    self.next_stream_id += 1;

                    let (stream, ctx) = Stream::new(id, self.cnx);
                    assert!(self.streams.insert(id, ctx).is_none());

                    let _ = sender.send(stream);
                }
            }
        }
    }

    fn close(&mut self) {
        //TODO maybe replace 0 with an appropriate error code
        unsafe {
            picoquic_close(self.cnx, 0);
        }

        self.closed = true;
        let _ = self.send_msg.unbounded_send(Message::Close);
    }
}

impl Future for Context {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        if self.closed {
            return Ok(Ready(()));
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
        ctx.borrow_mut().close();

    // when Rc goes out of scope, it will dereference the Context pointer automatically
    } else {
        let data = slice::from_raw_parts(bytes, length as usize);

        ctx.borrow_mut().recv_data(stream_id, data, event);

        // the context must not be dereferenced!
        mem::forget(ctx);
    }
}

#[derive(Clone)]
pub struct NewStream {
    send: UnboundedSender<(stream::Type, oneshot::Sender<Stream>)>,
}

impl NewStream {
    pub fn new_bidirectional_stream(&mut self) -> NewStreamFuture {
        self.new_stream(stream::Type::Bidirectional)
    }

    pub fn new_unidirectional_stream(&mut self) -> NewStreamFuture {
        self.new_stream(stream::Type::Unidirectional)
    }

    fn new_stream(&mut self, stype: stream::Type) -> NewStreamFuture {
        let (send, recv) = oneshot::channel();

        let _ = self.send.unbounded_send((stype, send));

        NewStreamFuture { recv }
    }
}

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
