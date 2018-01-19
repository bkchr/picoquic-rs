use stream;

pub(crate) struct Connection {}

impl Connection {
    pub fn new() -> Connection {
        Connection {}
    }

    pub fn recv_data(&self, stream_id: stream::Id, data: &[u8]) {}
}
