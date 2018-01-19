use stream::{self, Stream};

use picoquic_sys::picoquic::{self, picoquic_call_back_event_t, picoquic_cnx_t,
                             picoquic_set_callback};

use futures::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

use std::collections::HashMap;
use std::collections::hash_map::Entry::{Occupied, Vacant};
use std::mem;

use bytes::Bytes;

enum Message {
    NewStream(Stream),
    Close,
}

pub struct Connection {
    msg_recv: UnboundedReceiver<Message>,
}

impl Connection {
    pub fn recv_data(&self, stream_id: stream::Id, data: &[u8]) {}
}

impl From<(*mut picoquic_cnx_t, stream::Id, *mut u8, usize, picoquic_call_back_event_t)> for Connection {
    fn from(data: (*mut picoquic_cnx_t, stream::Id, *mut u8, usize, picoquic_call_back_event_t)) -> Connection {
        let (sender, msg_recv) = unbounded();

        let con = Connection { msg_recv };
        let ctx = Context::new(data.0, sender);

        // Now we need to call the callback once manually to process the received data
        recv_data_callback(data.0, data.1, data.2, data.3, ctx);

        con
    }
}

struct Context {
    send_msg: UnboundedSender<Message>,
    streams: HashMap<stream::Id, stream::Context>,
    cnx: *mut picoquic_cnx_t,
}

impl Context {
    fn new(cnx: *mut picoquic_cnx_t, msg_send: UnboundedSender<Message>) -> *mut c_void {
        let ctx = Box::new(Context { msg_send, streams: Default::default(), cnx });

        // Convert the `Context` to a `*mut c_void` and reset the callback to the
        // `recv_data_callback`
        unsafe {
            let ctx = Box::into_raw(ctx) as *mut c_void;
            picoquic_set_callback(cnx, Some(recv_data_callback), c_ctx);
            ctx
        }
    }

    fn recv_data(&mut self, id: stream::Id, data: &[u8], event: picoquic_call_back_event_t) {
        let new_stream = match self.streams.entry(id) {
            Occupied(entry) => {
                entry.get_mut().recv_data(self.cnx, data, event);
                None
            },
            Vacant(entry) => {
                let (stream, mut ctx) = Stream::new(id);

                ctx.recv_data(self.cnx, data, event);
                entry.insert(ctx);
                Some(stream)
            }
        };

        if let Some(stream) = new_stream {
            if self.send_msg.unbounded_send(Message::NewStream(stream)).is_err() {
                info!("will close connection, because `Connection` instance was dropped.");
                self.close();
            }
        }
    }

    fn close(&self) {
        //TODO maybe replace 0 with an appropriate error code
        unsafe { picoquic_close(self.cnx, 0); }
    }
}

impl From<*mut c_void> for Box<Context> {
    fn from(ptr: *mut c_void) -> Box<Context> {
        unsafe { Box::from_raw(ptr as *mut Context) }
    }
}

unsafe extern "C" fn recv_data_callback(
    cnx: *mut picoquic_cnx_t,
    stream_id: stream::Id,
    bytes: *mut u8,
    length: usize,
    event: picoquic_call_back_event_t,
    ctx: *mut c_void,
) {
    assert!(!ctx.is_null());
    let mut ctx = Box<Context>::from(ctx);

    if event == picoquic::picoquic_call_back_event_t_picoquic_callback_close
        || event == picoquic::picoquic_call_back_event_t_picoquic_callback_application_close
    {
        // when Box goes out of scope, it will delete the Context pointer automatically
    } else {
        let data = slice::from_raw_parts(bytes, length as usize);

        ctx.recv_data(stream_id, data, event);

        // the context must not be deleted!
        mem::forget(ctx);
    }
}
