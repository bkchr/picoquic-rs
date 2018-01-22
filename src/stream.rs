use futures::sync::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};

use bytes::{Bytes};

use futures::{Future, Poll, Stream as FStream};
use futures::Async::Ready;

use picoquic_sys::picoquic::{self, picoquic_call_back_event_t, picoquic_cnx_t,
                             picoquic_add_to_stream, picoquic_reset_stream};

pub type Id = u64;

pub enum Message {
    Close,
    Data(Bytes),
}

pub struct Stream {
    recv_msg: UnboundedReceiver<Message>,
    send_msg: UnboundedSender<Message>,
}

impl Stream {
    pub(crate) fn new(id: Id, cnx: *mut picoquic_cnx_t) -> (Stream, Context) {
        let (recv_msg, recv_send) = unbounded();
        let (send_msg, send_recv) = unbounded();

        let ctx = Context::new(recv_msg, send_recv, id, cnx);
        let stream = Stream {
            recv_msg: recv_send,
            send_msg: send_msg,
        };

        (stream, ctx)
    }
}

pub(crate) struct Context {
    recv_msg: UnboundedSender<Message>,
    send_msg: UnboundedReceiver<Message>,
    id: Id,
    finished: bool,
    cnx: *mut picoquic_cnx_t,
}

impl Context {
    pub fn new(
        recv_msg: UnboundedSender<Message>,
        mut send_msg: UnboundedReceiver<Message>,
        id: Id,
        cnx: *mut picoquic_cnx_t,
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
        }
    }

    fn reset(&mut self) {
        self.finished = true;
        unsafe {
            picoquic_reset_stream(self.cnx, self.id, 0);
        }
    }

    pub fn recv_data(&mut self, data: &[u8], event: picoquic_call_back_event_t) {
        if self.finished {
            error!("stream({}) received data after being finished!", self.id);
        } else if event == picoquic::picoquic_call_back_event_t_picoquic_callback_stop_sending
            || event == picoquic::picoquic_call_back_event_t_picoquic_callback_stream_reset
        {
            self.reset();
            let _ = self.recv_msg.unbounded_send(Message::Close);
        } else {
            let data = Bytes::from(data);

            let _ = self.recv_msg.unbounded_send(Message::Data(data));
        }
    }

    fn send_data(&mut self, data: Bytes) {
        //TODO: `set_fin`(last argument) should be configurable
        unsafe {
            picoquic_add_to_stream(self.cnx, self.id, data.as_ptr(), data.len(), 0);
        }
    }
}

impl Future for Context {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            match try_ready!(self.send_msg.poll()) {
                Some(Message::Close) => {
                    self.reset();
                    return Ok(Ready(()));
                }
                Some(Message::Data(data)) => {
                    self.send_data(data);
                }
                None => {
                    error!("received `None`, closing!");
                    self.reset();
                    return Ok(Ready(()));
                }
            }
        }
    }
}
