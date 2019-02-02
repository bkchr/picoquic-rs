use crate::channel_with_error::{
    channel as channel_with_error, Receiver as ReceiverWithError, SendError,
    Sender as SenderWithError,
};
use crate::error::*;
use crate::ffi;
use crate::swappable_stream::SwappableStream;
use picoquic_sys::picoquic::{
    self, picoquic_call_back_event_t, picoquic_mark_active_stream,
    picoquic_provide_stream_data_buffer, picoquic_reset_stream, picoquic_stop_sending,
};

use bytes::{Bytes, BytesMut};

use futures::{
    sync::mpsc::{unbounded, UnboundedReceiver, UnboundedSender},
    Async::{NotReady, Ready},
    Future, Poll, Sink, StartSend, Stream as FStream,
};

use std::{cmp, net::SocketAddr, slice};

use smallvec::SmallVec;

pub type Id = u64;

/// A `Message` is used by the `Stream` to propagate information from the peer or to send
/// information to the peer.
#[derive(Debug)]
enum Message {
    /// Close the `Stream`.
    Close,
    /// Recv data.
    RecvData(BytesMut),
    /// An error occurred.
    Error(Error),
    /// Reset the `Stream`.
    Reset,
    /// Swap the `send_data` receiver.
    ///
    /// This also carries the old `sender` as this one is not allowed to be dropped, before we set
    /// the new receiver as swappable.
    SwapSendData((ReceiverWithError<Bytes>, SenderWithError<Bytes>)),
}

/// A `Stream` can either be unidirectional or bidirectional.
#[derive(Copy, Clone)]
pub enum Type {
    Unidirectional,
    Bidirectional,
}

/// Create send and receive data channel.
///
/// If the given `Stream` id is unidirectional, `(None, None)` is returned.
fn create_send_and_recv_data(
    id: Id,
    client_con: bool,
    size: usize,
) -> (
    Option<SenderWithError<Bytes>>,
    Option<ReceiverWithError<Bytes>>,
) {
    if is_unidirectional(id) && !is_unidirectional_send_allowed(id, client_con) {
        (None, None)
    } else {
        let (sender, receiver) = channel_with_error(size);
        (Some(sender), Some(receiver))
    }
}

/// A `Stream` is part of a `Connection`. A `Connection` can consists of multiple `Stream`s.
/// Each `Stream` is a new channel over the `Connection` to the Peer. All traffic of a `Stream`
/// is always unique for each `Stream`.
/// The `Stream` needs to be polled, to get notified about a new `Message`.
#[derive(Debug)]
pub struct Stream {
    recv_msg: UnboundedReceiver<Message>,
    control_msg: UnboundedSender<Message>,
    send_data: Option<SenderWithError<Bytes>>,
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
        send_channel_default_size: usize,
    ) -> (Stream, Context) {
        let (recv_msg, recv_send) = unbounded();
        let (control_msg, recv_control) = unbounded();
        let (send_data, recv_data) =
            create_send_and_recv_data(id, is_client_con, send_channel_default_size);

        let ctx = Context::new(recv_msg, recv_control, recv_data, id, cnx, is_client_con);
        let stream = Stream {
            recv_msg: recv_send,
            control_msg,
            send_data,
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

    /// Resets this stream.
    pub fn reset(&mut self) -> Result<(), Error> {
        self.control_msg
            .unbounded_send(Message::Reset)
            .map_err(|_| ErrorKind::Unknown.into())
    }

    /// Returns if this stream received a reset.
    pub fn is_reset(&self) -> bool {
        self.stream_reset
    }

    /// Returns the `send_data` channel.
    fn get_send_data(&mut self) -> Result<&mut SenderWithError<Bytes>, Error> {
        self.send_data
            .as_mut()
            .ok_or_else(|| ErrorKind::SendOnUnidirectional.into())
    }

    /// Set the size of the send channel.
    ///
    pub fn set_send_channel_size(&mut self, size: usize) {
        if let Some(old_sender) = self.send_data.take() {
            let (sender, receiver) = channel_with_error(size);
            self.send_data = Some(sender);
            let _ = self
                .control_msg
                .unbounded_send(Message::SwapSendData((receiver, old_sender)));
        }
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
            Some(Message::Error(err)) => Err(err),
            Some(Message::Reset) => {
                self.stream_reset = true;
                Ok(Ready(None))
            }
            Some(Message::SwapSendData(_)) => panic!("SwapSendData received!"),
        }
    }
}

impl Sink for Stream {
    type SinkItem = Bytes;
    type SinkError = Error;

    fn start_send(&mut self, item: Self::SinkItem) -> StartSend<Self::SinkItem, Self::SinkError> {
        self.get_send_data()?.start_send(item).map_err(|e| match e {
            SendError::Channel(e, _) => e,
            SendError::Normal(e) => ErrorKind::SendError(e.into_inner()).into(),
        })
    }

    fn poll_complete(&mut self) -> Poll<(), Self::SinkError> {
        self.get_send_data()?
            .poll_complete()
            .map_err(|_| ErrorKind::Unknown.into())
    }
}

pub(crate) struct Context {
    recv_msg: UnboundedSender<Message>,
    send_data: Option<SwappableStream<ReceiverWithError<Bytes>>>,
    control_msg: UnboundedReceiver<Message>,
    id: Id,
    cnx: ffi::Connection,
    /// Is the connection this Stream belongs to, a client connection?
    is_client_con: bool,
    /// Did this stream send any data?
    data_send: bool,

    stop_sending_or_fin_sent: bool,

    fin_received_or_recv_msg_dropped: bool,

    /// The active buffer that is currently fetched when this stream wants to send data.
    active_buffer: Option<Bytes>,
}

impl Context {
    fn new(
        recv_msg: UnboundedSender<Message>,
        mut control_msg: UnboundedReceiver<Message>,
        mut send_data: Option<ReceiverWithError<Bytes>>,
        id: Id,
        cnx: ffi::Connection,
        is_client_con: bool,
    ) -> Context {
        let _ = control_msg.poll();
        send_data.as_mut().map(|s| {
            let _ = s.poll();
        });

        let fin_received_or_recv_msg_dropped =
            is_unidirectional(id) && is_unidirectional_send_allowed(id, is_client_con);
        let stop_sending_or_fin_sent =
            is_unidirectional(id) && !is_unidirectional_send_allowed(id, is_client_con);

        Context {
            recv_msg,
            control_msg,
            send_data: send_data.map(|s| s.into()),
            id,
            cnx,
            is_client_con,
            data_send: false,
            stop_sending_or_fin_sent,
            fin_received_or_recv_msg_dropped,
            active_buffer: None,
        }
    }

    fn close_send_data(&mut self) {
        self.send_data.take().map(|mut s| s.close());
    }

    fn reset(&mut self) {
        self.stop_sending_or_fin_sent = true;
        self.close_send_data();

        unsafe {
            picoquic_reset_stream(self.cnx.as_ptr(), self.id, 0);
        }
    }

    fn recv_message(&mut self, msg: Message) {
        if self.recv_msg.unbounded_send(msg).is_err() {
            self.recv_message_dropped();
        }
    }

    fn recv_message_dropped(&mut self) {
        if !self.fin_received_or_recv_msg_dropped {
            self.fin_received_or_recv_msg_dropped = true;

            if !is_unidirectional(self.id)
                || !is_unidirectional_send_allowed(self.id, self.is_client_con)
            {
                unsafe {
                    picoquic_stop_sending(self.cnx.as_ptr(), self.id, 0);
                }
            }
        }
    }

    pub fn handle_callback(
        &mut self,
        ptr: *mut u8,
        length: usize,
        event: picoquic_call_back_event_t,
    ) {
        if event == picoquic::picoquic_call_back_event_t_picoquic_callback_prepare_to_send {
            self.handle_send_data(ptr, length);
        } else {
            let data = unsafe { slice::from_raw_parts(ptr, length) };

            if !data.is_empty() && !self.fin_received_or_recv_msg_dropped {
                let data = BytesMut::from(data);
                self.recv_message(Message::RecvData(data));
            }

            if event == picoquic::picoquic_call_back_event_t_picoquic_callback_stream_reset {
                self.fin_received_or_recv_msg_dropped = true;
                self.recv_message(Message::Reset);
            } else if event == picoquic::picoquic_call_back_event_t_picoquic_callback_stop_sending {
                self.stop_sending_or_fin_sent = true;
                self.close_send_data();
            } else if event == picoquic::picoquic_call_back_event_t_picoquic_callback_stream_fin {
                self.fin_received_or_recv_msg_dropped = true;
                self.recv_message(Message::Close);
            }
        }
    }

    /// Handle a connection error.
    pub fn handle_connection_error(&mut self, err: impl ErrorFn<Output = Error>) {
        self.recv_message(Message::Error(err()));
        self.send_data.as_mut().map(|s| s.propagate_error(err));
    }

    /// Handle connection close.
    pub fn handle_connection_close(&mut self) {
        self.recv_message(Message::Close);
    }

    /// Collect buffers into the given `buffers` parameter that should be send.
    ///
    /// # Returns
    ///
    /// Returns the size of all collected buffers and if the `fin` bit should be set.
    fn collect_buffers_to_send(
        &mut self,
        max_length: usize,
        buffers: &mut SmallVec<[Bytes; 32]>,
    ) -> (usize, bool) {
        let mut size = 0;
        let mut fin = false;

        if let Some(buf) = self.active_buffer.take() {
            size += buf.len();
            buffers.push(buf);
        }

        while size <= max_length {
            match self.send_data.as_mut() {
                Some(ref mut recv) => {
                    match recv.poll().expect("Receiver never returns an error.") {
                        Ready(Some(buf)) => {
                            size += buf.len();
                            buffers.push(buf);
                        }
                        Ready(None) => {
                            fin = true;
                            break;
                        }
                        NotReady => break,
                    }
                }
                None => {
                    fin = true;
                    break;
                }
            }
        }

        (size, fin)
    }

    /// Picoquic wants to send data and our `Stream` was marked as active.
    fn handle_send_data(&mut self, ctx: *mut u8, max_length: usize) {
        let mut buffers = SmallVec::new();
        let (size, fin) = self.collect_buffers_to_send(max_length, &mut buffers);

        self.data_send = self.data_send || size > 0;
        let still_active = size > max_length && !fin;
        let length = cmp::min(max_length, size);
        let dest_slice = unsafe {
            let dest = picoquic_provide_stream_data_buffer(
                ctx as _,
                length,
                fin as i32,
                still_active as i32,
            );

            if dest.is_null() {
                panic!("Stream data buffer should never be NULL.");
            }

            slice::from_raw_parts_mut(dest, length)
        };

        self.stop_sending_or_fin_sent = fin;

        let mut written = 0;
        buffers.into_iter().for_each(|mut buf| {
            if written < length {
                let end = cmp::min(written + buf.len(), length);
                let buf_end = end - written;
                dest_slice[written..end].copy_from_slice(&buf[0..buf_end]);
                buf.advance(end - written);
                written = end;
            }

            if !buf.is_empty() && self.active_buffer.is_none() {
                self.active_buffer = Some(buf);
            } else if !buf.is_empty() {
                panic!("Active buffer should never be set twice!");
            }
        });
    }

    fn handle_send_data_dropped(&mut self) {
        self.send_data.take();

        if !self.data_send && self.active_buffer.is_none() {
            self.reset();
        } else if !self.stop_sending_or_fin_sent {
            unsafe {
                // We will need to set the fin bit
                picoquic_mark_active_stream(self.cnx.as_ptr(), self.id, 1);
            }
        }
    }

    fn poll_control_msg(&mut self) -> Poll<(), Error> {
        loop {
            match try_ready!(self
                .control_msg
                .poll()
                .map_err(|_| Error::from(ErrorKind::Unknown)))
            {
                Some(Message::Reset) => {
                    self.reset();
                }
                Some(Message::SwapSendData((new_receiver, _old_sender))) => {
                    // `_old_sender` can be dropped after we set the receiver to swap to
                    self.send_data.as_mut().map(|s| s.set_swap_to(new_receiver));
                }
                None => {
                    self.recv_message_dropped();
                    return Ok(Ready(()));
                }
                r => panic!("Stream context unknown `Message`: {:?}", r),
            }
        }
    }

    fn poll_send_data(&mut self) -> Result<(), Error> {
        if let (None, Some(ref mut recv)) = (&self.active_buffer, self.send_data.as_mut()) {
            match recv.poll()? {
                Ready(Some(buf)) => {
                    self.active_buffer = Some(buf);
                    unsafe {
                        picoquic_mark_active_stream(self.cnx.as_ptr(), self.id, 1);
                    }
                }
                Ready(None) => self.handle_send_data_dropped(),
                _ => {}
            }
        }

        Ok(())
    }
}

fn is_unidirectional(id: Id) -> bool {
    id & 2 != 0
}

/// Returns if this Stream is the sending side of an unidirectional Stream.
fn is_unidirectional_send_allowed(id: Id, is_client_con: bool) -> bool {
    if is_client_initiated(id) {
        is_client_con
    } else {
        !is_client_con
    }
}

/// Is the Stream initiated by the client?
fn is_client_initiated(id: Id) -> bool {
    id & 1 == 0
}

impl Future for Context {
    type Item = ();
    type Error = Error;

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        self.poll_control_msg()?;
        self.poll_send_data()?;

        if self.stop_sending_or_fin_sent && self.fin_received_or_recv_msg_dropped {
            Ok(Ready(()))
        } else {
            Ok(NotReady)
        }
    }
}

impl Drop for Context {
    fn drop(&mut self) {
        let _ = self.recv_msg.unbounded_send(Message::Close);
        self.close_send_data();
    }
}
