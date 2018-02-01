use error::*;
use super::packet::Packet;
use super::quic_ctx::{socket_addr_from_c, QuicCtx};
use stream;

use picoquic_sys::picoquic::{self, picoquic_close, picoquic_cnx_t, picoquic_create_cnx,
                             picoquic_delete_cnx, picoquic_get_cnx_state, picoquic_get_first_cnx,
                             picoquic_get_next_cnx, picoquic_get_peer_addr, picoquic_quic_t,
                             picoquic_state_enum_picoquic_state_client_ready,
                             picoquic_state_enum_picoquic_state_disconnected,
                             picoquic_state_enum_picoquic_state_server_ready};

use std::net::SocketAddr;
use std::ptr;

use socket2::SockAddr;

#[derive(Copy, Clone)]
pub struct Connection {
    cnx: *mut picoquic_cnx_t,
}

impl Connection {
    pub fn new(
        quic: &QuicCtx,
        server_addr: SocketAddr,
        current_time: u64,
    ) -> Result<Connection, Error> {
        assert!(
            !server_addr.ip().is_unspecified(),
            "server address must not be unspecified!"
        );

        let server_addr = SockAddr::from(server_addr);

        let cnx = unsafe {
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
        };

        if cnx.is_null() {
            Err(ErrorKind::Unknown)?;
        }

        Ok(Connection { cnx })
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

    /// Is the connection ready to be used?
    pub fn is_ready(&self) -> bool {
        let state = self.get_state();
        state == picoquic_state_enum_picoquic_state_client_ready
            || state == picoquic_state_enum_picoquic_state_server_ready
    }

    fn get_state(&self) -> u32 {
        unsafe { picoquic_get_cnx_state(self.cnx) }
    }

    pub fn close(&self) {
        //TODO maybe replace 0 with an appropriate error code
        unsafe {
            picoquic_close(self.cnx, 0);
        }
    }

    /// Generates a new `Stream` id from the given `next_id`. The `next_id` can be incremented by
    /// one, after calling this function. The resulting `Stream` id depends on `is_client` and
    /// `stype`, as both values are encoded in the first two bits of the new id.
    pub(crate) fn generate_stream_id(
        next_id: u64,
        is_client: bool,
        stype: stream::Type,
    ) -> stream::Id {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_bidirectional_stream_id_generation() {
        assert_eq!(
            4,
            Connection::generate_stream_id(0, true, stream::Type::Bidirectional)
        );
        assert_eq!(
            8,
            Connection::generate_stream_id(1, true, stream::Type::Bidirectional)
        );
        assert_eq!(
            12,
            Connection::generate_stream_id(2, true, stream::Type::Bidirectional)
        );
    }

    #[test]
    fn client_unidirectional_stream_id_generation() {
        assert_eq!(
            6,
            Connection::generate_stream_id(0, true, stream::Type::Unidirectional)
        );
        assert_eq!(
            10,
            Connection::generate_stream_id(1, true, stream::Type::Unidirectional)
        );
        assert_eq!(
            14,
            Connection::generate_stream_id(2, true, stream::Type::Unidirectional)
        );
    }

    #[test]
    fn server_bidirectional_stream_id_generation() {
        assert_eq!(
            5,
            Connection::generate_stream_id(0, false, stream::Type::Bidirectional)
        );
        assert_eq!(
            9,
            Connection::generate_stream_id(1, false, stream::Type::Bidirectional)
        );
        assert_eq!(
            13,
            Connection::generate_stream_id(2, false, stream::Type::Bidirectional)
        );
    }

    #[test]
    fn server_unidirectional_stream_id_generation() {
        assert_eq!(
            7,
            Connection::generate_stream_id(0, false, stream::Type::Unidirectional)
        );
        assert_eq!(
            11,
            Connection::generate_stream_id(1, false, stream::Type::Unidirectional)
        );
        assert_eq!(
            15,
            Connection::generate_stream_id(2, false, stream::Type::Unidirectional)
        );
    }

    #[test]
    #[should_panic(expected = "server address must not be unspecified!")]
    fn do_not_accept_unspecified_ip_address() {
        let _ = Connection::new(&QuicCtx::dummy(), ([0, 0, 0, 0], 12345).into(), 0);
    }
}
