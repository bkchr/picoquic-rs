use error::Error;
use super::packet::Packet;
use super::quic_ctx::{socket_addr_from_c, QuicCtx};
use stream;

use picoquic_sys::picoquic::{self, picoquic_cnx_t, picoquic_create_cnx, picoquic_delete_cnx,
                             picoquic_get_cnx_state, picoquic_get_first_cnx,
                             picoquic_get_next_cnx, picoquic_get_peer_addr, picoquic_quic_t,
                             picoquic_state_enum_picoquic_state_disconnected};

use std::net::SocketAddr;
use std::ptr;

use socket2::SockAddr;

pub struct Connection {
    cnx: *mut picoquic_cnx_t,
}

impl Connection {
    pub fn new(quic: &QuicCtx, server_addr: SocketAddr, current_time: u64) -> *mut picoquic_cnx_t {
        let server_addr = SockAddr::from(server_addr);

        unsafe {
            picoquic_create_cnx(
                quic.as_ptr(),
                0,
                server_addr.as_ptr() as *mut picoquic::sockaddr,
                current_time,
                0,
                ptr::null_mut(),
                ptr::null_mut(),
                1,
            )
        }
    }

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

    pub(crate) fn get_stream_id(next_id: u64, is_client: bool, stype: stream::Type) -> u64 {
        // Stream 0, 1, 2 and 3 are reserved.
        // Client first usable stream is 4, Server first usable stream is 5.
        // Client gets even stream ids and server gets odd stream ids.
        let mut id = next_id + 1;

        id <<= 2;

        if !is_client {
            id |= 1;
        }

        // Unidirectional sets the second bit to 1
        if let stream::Type::Unidirectional = stype {
            id |= 2;
        }
        
        id
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

