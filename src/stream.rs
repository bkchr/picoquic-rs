use futures::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

use bytes::{Bytes, BytesMut};

use futures::Future;

pub type Id = u64;

pub enum Message {
    Close,
    Data(Bytes),
}

struct Stream {
    recv_msg: UnboundedReceiver<Message>,
    send_msg: UnboundedSender<Message>,
}

impl Stream {
    pub fn new(id: Id) -> (Stream, Context) {
        let (recv_msg, recv_send) = unbounded();
        let (send_msg, send_recv) = unbounded();

        let ctx = Context::new(recv_send, send_recv, id);
        let stream = Stream { recv_data, send_data };

        (stream, context)
    }
}

pub(crate) struct Context {
    recv_msg: UnboundedSender<Bytes>,
    send_msg: UnboundedReceiver<BytesMut>,
    id: Id,    
    finished: bool,
    cnx: *mut picoquic_cnx_t, 
}

impl Context {
    pub fn new(recv_msg: UnboundedSender<Message>, send_msg: UnboundedReceiver<Message>, id: Id) -> Context {
        // We need to poll this once, so the current `Task` is registered to be woken up, when
        // new data should be send.
        let _ = send_msg.poll();

        Context {
            recv_msg,
            send_msg,
            id,
            finished: false,
        }
    }

    fn reset(&mut self) {
        self.finished = true;
        unsafe { picoquic_reset_stream(self.cnx, self.id, 0); }
    }

    fn recv_data(&mut self, data: &[u8], event: picoquic_call_back_event_t) {
        if self.finished {
            error!("stream({}) received data after being finished!", self.id);
        } else if event == picoquic::picoquic_call_back_event_t_picoquic_callback_stop_sending ||
            event == picoquic::picoquic_call_back_event_t_picoquic_callback_stream_reset {
                self.reset();
                                let _ = self.recv_msg.unbounded_send(Message::Close);
            } else {
                let data = Bytes::from(data);

                let _ = self.recv_msg.unbounded_send(Message::Data(data));
            }
    }
}

impl Future for Context {
    type Item = ();
    type Error = ();

    fn poll(&mut self) -> Poll<Self::Item, Self::Error> {
        loop {
            match try_ready!(self.send_msg) {
                Some(Message::Close) => {
                    self.reset();
                    return Ok(());
                },
                Some(Message::Data(data)) => {
                    
                },
                None => {
                    error!("received `None`, closing!");
                    self.reset();
                    return Ok(());
                }
            }
        }
    }
}
