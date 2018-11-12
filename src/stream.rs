use error::*;
use ffi;
use picoquic_sys::picoquic::{
    self, picoquic_add_to_stream, picoquic_call_back_event_t, picoquic_reset_stream,
    picoquic_stop_sending,
};

use bytes::{Bytes, BytesMut};

use futures::{
    sync::{
        mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
        oneshot,
    },
    Async::{NotReady, Ready},
    Future, Poll, Sink, StartSend, Stream as FStream,
};

use std::{net::SocketAddr, ptr};

pub type Id = u64;

/// A `Message` is used by the `Stream` to propagate information from the peer or to send
/// information to the peer.
#[derive(Debug)]
enum Message {
    /// Close the `Stream`.
    Close,
    /// Recv data.
    RecvData(BytesMut),
    /// Send data.
    SendData(Bytes),
    Error(Error),
    /// Reset the `Stream`.
    Reset,
}

/// A `Stream` can either be unidirectional or bidirectional.
#[derive(Copy, Clone)]
pub enum Type {
    Unidirectional,
    Bidirectional,
}

/// A `Stream` is part of a `Connection`. A `Connection` can consists of multiple `Stream`s.
/// Each `Stream` is a new channel over the `Connection` to the Peer. All traffic of a `Stream`
/// is always unique for each `Stream`.
/// The `Stream` needs to be polled, to get notified about a new `Message`.
#[derive(Debug)]
pub struct Stream {
    recv_msg: UnboundedReceiver<Message>,
    send_msg: SenderWithError,
    id: Id,
    peer_addr: SocketAddr,
    local_addr: SocketAddr,
    stream_reset: bool,
}

impl Stream {
    pub(crate) fn new(
        id: Id,
        cnx: ffi::Connection,
        local_addr: SocketAddr,
        is_client_con: bool,
    ) -> (Stream, Context) {
        let (recv_msg, recv_send) = unbounded();
        let (send_msg, send_recv) = channel_with_error();

        let ctx = Context::new(recv_msg, send_recv, id, cnx, is_client_con);
        let stream = Stream {
            recv_msg: recv_send,
            send_msg,
            id,
            peer_addr: cnx.peer_addr(),
            local_addr,
            stream_reset: false,
        };

        (stream, ctx)
    }

    /// Returns the type of this `Stream`, either `Type::Unidirectional` or `Type::Bidirectional`.
    pub fn get_type(&self) -> Type {
        if is_unidirectional(self.id) {
            Type::Unidirectional
        } else {
            Type::Bidirectional
        }
    }

    /// Returns the address of the `Connection`'s peer.
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// Returns the address of the `Connection`'s local `Context`, where it is listening on.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Resets this stream (immediate termination).
    pub fn reset(mut self) -> Result<(), Error> {
        self.send_msg
            .start_send(Message::Reset)
            .map(|_| ())
            .map_err(|e| e.into_error(|_| ErrorKind::Unknown.into()))
    }

    /// Returns if this stream received a reset.
    pub fn is_reset(&self) -> bool {
        self.stream_reset
    }
}

impl FStream for Stream {
    type Item = BytesMut;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        match try_ready!(self
            .recv_msg
            .poll()
            .map_err(|_| Error::from(ErrorKind::Unknown)))
        {
            Some(Message::Close) | None => Ok(Ready(None)),
            Some(Message::RecvData(d)) => Ok(Ready(Some(d))),
            Some(Message::SendData(_)) => panic!("`SendData` message in `Stream` poll!"),
            Some(Message::Error(err)) => Err(err),
            Some(Message::Reset) => {
                self.stream_reset = true;
                Ok(Ready(None))
            }
        }
    }
}

impl Sink for Stream {
    type SinkItem = Bytes;
    type SinkError = Error;

    fn start_send(&mut self, item: Self::SinkItem) -> StartSend<Self::SinkItem, Self::SinkError> {
        fn extract_data(val: Message) -> Bytes {
            match val {
                Message::SendData(d) => d,
                _ => unreachable!(),
            }
        }

        self.send_msg
            .start_send(Message::SendData(item))
            .map_err(|e| e.into_error(|item| ErrorKind::SendError(extract_data(item)).into()))
            .map(|r| r.map(extract_data))
    }

    fn poll_complete(&mut self) -> Poll<(), Self::SinkError> {
        self.send_msg
            .poll_complete()
            .map_err(|_| ErrorKind::Unknown.into())
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        let _ = self.send_msg.start_send(Message::Close);
    }
}

pub(crate) struct Context {
    recv_msg: UnboundedSender<Message>,
    send_msg: ReceiverWithError,
    id: Id,
    finished: bool,
    cnx: ffi::Connection,
    /// Is the connection this Stream belongs to, a client connection?
    is_client_con: bool,
    /// Did this stream send any data?
    data_send: bool,
    stop_sending: bool,
}

impl Context {
    fn new(
        recv_msg: UnboundedSender<Message>,
        mut send_msg: ReceiverWithError,
        id: Id,
        cnx: ffi::Connection,
        is_client_con: bool,
    ) -> Context {
        // We need to poll this once, so the current `Task` is registered to be woken up, when
        // new data should be send.
        let _ = send_msg.poll();

        Context {
            recv_msg,
            send_msg,
            id,
            finished: false,
            cnx,
            is_client_con,
            data_send: false,
            stop_sending: false,
        }
    }

    fn reset(&mut self) {
        self.finished = true;
        unsafe {
            picoquic_reset_stream(self.cnx.as_ptr(), self.id, 0);
        }
    }

    pub fn recv_data(&mut self, data: &[u8], event: picoquic_call_back_event_t) {
        if !data.is_empty() {
            if self.finished {
                error!("stream({}) received data after being finished!", self.id);
            } else {
                let data = BytesMut::from(data);

                let _ = self.recv_msg.unbounded_send(Message::RecvData(data));
            }
        }

        if event == picoquic::picoquic_call_back_event_t_picoquic_callback_stream_reset {
            self.finished = true;
            let _ = self.recv_msg.unbounded_send(Message::Reset);
        } else if event == picoquic::picoquic_call_back_event_t_picoquic_callback_stop_sending {
            self.stop_sending = true;
            self.send_msg.close();
        } else if event == picoquic::picoquic_call_back_event_t_picoquic_callback_stream_fin {
            let _ = self.recv_msg.unbounded_send(Message::Close);
            self.finished = true;
        }
    }

    /// Handle a connection error.
    pub fn handle_connection_error(&mut self, err: &Fn() -> Error) {
        let _ = self.recv_msg.unbounded_send(Message::Error(err()));
        self.send_msg.propagate_error(err())
    }

    /// Handle connection close.
    pub fn handle_connection_close(&mut self) {
        let _ = self.recv_msg.unbounded_send(Message::Close);
    }

    fn send_data(&mut self, data: Bytes) {
        if is_unidirectional(self.id) && !self.is_unidirectional_send_allowed() {
            //TODO: maybe we should do more than just printing
            error!("tried to send data to incoming unidirectional stream!");
        } else if !self.stop_sending {
            self.data_send = self.data_send || !data.is_empty();
            unsafe {
                // TODO handle the result
                picoquic_add_to_stream(self.cnx.as_ptr(), self.id, data.as_ptr(), data.len(), 0);
            }
        }
    }

    fn close(&mut self) {
        self.finished = true;
        self.stop_sending = true;
        self.send_msg.close();

        if self.data_send {
            unsafe {
                picoquic_add_to_stream(self.cnx.as_ptr(), self.id, ptr::null(), 0, 1);
            }
        } else {
            self.reset();
        }

        if !is_unidirectional(self.id) || !self.is_unidirectional_send_allowed() {
            unsafe {
                picoquic_stop_sending(self.cnx.as_ptr(), self.id, 0);
            }
        }
    }

    /// Returns if this Stream is the sending side of an unidirectional Stream.
    fn is_unidirectional_send_allowed(&self) -> bool {
        if self.is_client_initiated() {
            self.is_client_con
        } else {
            !self.is_client_con
        }
    }

    /// Is the Stream initiated by the client?
    fn is_client_initiated(&self) -> bool {
        self.id & 1 == 0
    }
}

fn is_unidirectional(id: Id) -> bool {
    id & 2 != 0
}

impl Future for Context {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            match try_ready!(self.send_msg.poll()) {
                Some(Message::Reset) => {
                    self.reset();
                    return Ok(Ready(()));
                }
                Some(Message::Close) => {
                    self.close();
                    return Ok(Ready(()));
                }
                Some(Message::SendData(data)) => {
                    self.send_data(data);
                }
                Some(Message::RecvData(_)) => {
                    panic!("`RecvData` message in `Context` future!");
                }
                Some(Message::Error(_)) => {}
                None => {
                    if self.finished && self.stop_sending {
                        return Ok(Ready(()));
                    } else {
                        return Ok(NotReady);
                    }
                }
            }
        }
    }
}

/// Error used by `SenderWithError` to distinguish between propagated error and generic error.
enum SendError {
    Propagate(Error),
    Generic(Message),
}

impl SendError {
    fn into_error<T: Fn(Message) -> Error>(self, map_err: T) -> Error {
        match self {
            SendError::Propagate(err) => err,
            SendError::Generic(message) => map_err(message),
        }
    }
}

/// A sender with an associated error that can be set by the receiver side.
#[derive(Debug)]
struct SenderWithError {
    sender: UnboundedSender<Message>,
    error_recv: oneshot::Receiver<Error>,
    error_received: bool,
}

impl SenderWithError {
    fn new(sender: UnboundedSender<Message>, error_recv: oneshot::Receiver<Error>) -> Self {
        Self {
            sender,
            error_recv,
            error_received: false,
        }
    }

    fn check_for_error(&mut self) -> Option<Error> {
        // If `sender` is closed, check if we got an error to propagate
        if self.sender.is_closed() && !self.error_received {
            if let Ok(Some(err)) = self.error_recv.try_recv() {
                self.error_received = true;
                return Some(err);
            }
        }

        None
    }
}
impl Sink for SenderWithError {
    type SinkItem = Message;
    type SinkError = SendError;

    fn start_send(&mut self, item: Self::SinkItem) -> StartSend<Self::SinkItem, Self::SinkError> {
        self.sender.start_send(item).map_err(|e| {
            if let Some(err) = self.check_for_error() {
                SendError::Propagate(err)
            } else {
                SendError::Generic(e.into_inner())
            }
        })
    }
    fn poll_complete(&mut self) -> Poll<(), Self::SinkError> {
        self.sender
            .poll_complete()
            .map_err(|_| SendError::Propagate(ErrorKind::Unknown.into()))
    }
}

/// A receiver with the ability to propagate an error to the sender.
struct ReceiverWithError {
    receiver: UnboundedReceiver<Message>,
    error_sender: Option<oneshot::Sender<Error>>,
}

impl ReceiverWithError {
    fn new(receiver: UnboundedReceiver<Message>, error_sender: oneshot::Sender<Error>) -> Self {
        Self {
            receiver,
            error_sender: Some(error_sender),
        }
    }

    /// Propagate the given error to the `Sender` side.
    /// This will only work once, further calls of this function will just discard the error.
    fn propagate_error(&mut self, err: Error) {
        self.error_sender.take().map(|v| {
            let _ = v.send(err);
        });
    }

    fn close(&mut self) {
        self.receiver.close();
    }
}

impl FStream for ReceiverWithError {
    type Item = Message;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.receiver.poll().map_err(|_| ErrorKind::Unknown.into())
    }
}

fn channel_with_error() -> (SenderWithError, ReceiverWithError) {
    let (sender, receiver) = unbounded();
    let (err_sender, err_receiver) = oneshot::channel();

    (
        SenderWithError::new(sender, err_receiver),
        ReceiverWithError::new(receiver, err_sender),
    )
}
