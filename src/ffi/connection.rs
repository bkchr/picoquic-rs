use error::Error;
use super::packet::Packet;

use picoquic_sys::picoquic::{picoquic_cnx_t, picoquic_delete_cnx, picoquic_get_cnx_state,
                             picoquic_get_peer_addr,
                             picoquic_state_enum_picoquic_state_disconnected};

use std::net::SocketAddr;
use std::ptr;

use socket2::SockAddr;

use tokio_core::net::UdpSocket;

use libc;

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
        let mut peer_addr = ptr::null_mut();

        let addr = unsafe {
            picoquic_get_peer_addr(self.cnx, &mut peer_addr, &mut peer_addr_len);

            SockAddr::from_raw_parts(peer_addr as *const libc::sockaddr, peer_addr_len as u32)
        };

        addr.as_inet()
            .map(|v| v.into())
            .or(addr.as_inet6().map(|v| v.into()))
            .expect("neither ipv4 nor ipv6?")
    }

    /// Creates a `Packet` and sends it via the given `UdpSocket`.
    /// The `Packet` contains any data from this connection(data from streams, ACK's, ...).
    pub fn create_and_send_packet(
        &self,
        buffer: &mut [u8],
        socket: &mut UdpSocket,
        current_time: u64,
    ) -> Result<(), Error> {
        let packet = Packet::create(buffer, self, current_time)?;
        packet.send(socket, self.get_peer_addr())
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
