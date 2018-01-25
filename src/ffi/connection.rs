use error::Error;
use super::packet::Packet;
use super::quic_ctx::socket_addr_from_c;

use picoquic_sys::picoquic::{self, picoquic_cnx_t, picoquic_delete_cnx, picoquic_get_cnx_state,
                             picoquic_get_first_cnx, picoquic_get_next_cnx,
                             picoquic_get_peer_addr, picoquic_quic_t,
                             picoquic_state_enum_picoquic_state_disconnected};

use std::net::SocketAddr;
use std::ptr;

pub struct Connection {
    cnx: *mut picoquic_cnx_t,
}

impl Connection {
    pub fn as_ptr(&self) -> *mut picoquic_cnx_t {
        self.cnx
    }

    /// The peer address of this connection.
    pub fn get_peer_addr(&self) -> SocketAddr {
        let mut peer_addr_len = 0;
        let mut peer_addr: *mut picoquic::sockaddr = ptr::null_mut();

        unsafe {
            picoquic_get_peer_addr(self.cnx, &mut peer_addr, &mut peer_addr_len);

            socket_addr_from_c(peer_addr, peer_addr_len)
        }
    }

    /// Creates and prepares a `Packet`.
    /// The `Packet` contains any data from this connection(data from streams, ACK's, ...).
    /// The `Packet` will be stored in the given buffer.
    ///
    /// # Returns
    /// The length of the `Packet` in the buffer or `None` if the package does not contains any data.
    pub fn create_and_prepare_packet(
        &self,
        buffer: &mut [u8],
        current_time: u64,
    ) -> Result<Option<usize>, Error> {
        let mut packet = Packet::create(buffer)?;
        let size = packet.prepare(current_time, self)?;

        if packet.contains_data() {
            Ok(Some(size))
        } else {
            Ok(None)
        }
    }

    /// Deletes the underlying C pointer!
    pub fn delete(self) {
        unsafe {
            picoquic_delete_cnx(self.cnx);
        }
    }

    pub fn is_disconnected(&self) -> bool {
        self.get_state() == picoquic_state_enum_picoquic_state_disconnected
    }

    fn get_state(&self) -> u32 {
        unsafe { picoquic_get_cnx_state(self.cnx) }
    }
}

impl From<*mut picoquic_cnx_t> for Connection {
    fn from(cnx: *mut picoquic_cnx_t) -> Connection {
        Connection { cnx }
    }
}

pub struct ConnectionIter {
    current: *mut picoquic_cnx_t,
}

impl ConnectionIter {
    pub fn new(quic: *mut picoquic_quic_t) -> ConnectionIter {
        ConnectionIter {
            current: unsafe { picoquic_get_first_cnx(quic) },
        }
    }
}

impl Iterator for ConnectionIter {
    type Item = Connection;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.current.is_null() {
            let res = Some(Connection::from(self.current));

            self.current = unsafe { picoquic_get_next_cnx(self.current) };

            res
        } else {
            None
        }
    }
}
