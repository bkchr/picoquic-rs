use error::*;
use super::Connection;
use config::Config;

use picoquic_sys::picoquic::{self, picoquic_cnx_t, picoquic_create,
                             picoquic_delete_stateless_packet, picoquic_dequeue_stateless_packet,
                             picoquic_free, picoquic_get_first_cnx, picoquic_get_next_cnx,
                             picoquic_incoming_packet, picoquic_quic_t,
                             picoquic_stateless_packet_t, picoquic_stream_data_cb_fn};

use std::iter::Iterator;
use std::os::raw::c_void;
use std::ffi::CString;
use std::ptr;
use std::net::SocketAddr;
use std::mem;
use std::slice;

use socket2::SockAddr;

use tokio_core::net::UdpSocket;

use libc;

pub struct QuicCtx {
    quic: *mut picoquic_quic_t,
}

impl QuicCtx {
    pub fn new(
        config: Config,
        default_ctx: *mut c_void,
        default_callback: picoquic_stream_data_cb_fn,
        current_time: u64,
    ) -> Result<QuicCtx, Error> {
        // The number of buckets that picoquic will allocate for connections
        // The buckets itself are a linked list
        let connection_buckets = 16;

        let cert_filename = CString::new(config.cert_filename).context(ErrorKind::CStringError)?;
        let key_filename = CString::new(config.key_filename).context(ErrorKind::CStringError)?;
        let reset_seed = config
            .reset_seed
            .map(|mut v| v.as_mut_ptr())
            .unwrap_or_else(|| ptr::null_mut());

        let quic = unsafe {
            picoquic_create(
                connection_buckets,
                cert_filename.as_ptr(),
                ptr::null(),
                key_filename.as_ptr(),
                default_callback,
                default_ctx,
                None,
                ptr::null_mut(),
                reset_seed,
                current_time,
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                0,
            )
        };

        Ok(QuicCtx { quic })
    }

    pub fn connection_iter(&self) -> ConnectionIter {
        ConnectionIter::new(self.quic)
    }

    pub fn incoming_data(
        &mut self,
        buf: &mut [u8],
        addr_to: SocketAddr,
        addr_from: SocketAddr,
        current_time: u64,
    ) {
        let addr_to = SockAddr::from(addr_to);
        let addr_from = SockAddr::from(addr_from);

        unsafe {
            picoquic_incoming_packet(
                self.quic,
                buf.as_mut_ptr(),
                buf.len() as u32,
                addr_from.as_ptr() as *mut picoquic::sockaddr,
                addr_to.as_ptr() as *mut picoquic::sockaddr,
                // as long as we only support one udp socket, we don't need to change this index
                0,
                current_time,
            );
        }
    }

    pub fn stateless_packet_iter(&self) -> StatelessPacketItr {
        // TODO, ensure that the iterator lives not longer than the context(some lifetime magic)
        StatelessPacketItr::new(self.quic)
    }
    
}

impl Drop for QuicCtx {
    fn drop(&mut self) {
        unsafe {
            picoquic_free(self.quic);
        }
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

pub struct StatelessPacketItr {
    quic: *mut picoquic_quic_t,
}

impl StatelessPacketItr {
    fn new(quic: *mut picoquic_quic_t) -> StatelessPacketItr {
        StatelessPacketItr { quic }
    }
}

impl Iterator for StatelessPacketItr {
    type Item = StatelessPacket;

    fn next(&mut self) -> Option<Self::Item> {
        let res = unsafe { picoquic_dequeue_stateless_packet(self.quic) };
        if res.is_null() {
            None
        } else {
            Some(StatelessPacket::new(res))
        }
    }
}

pub fn socket_addr_from_c(sock_addr: *mut picoquic::sockaddr, sock_len: i32) -> SocketAddr {
    let addr =
        unsafe { SockAddr::from_raw_parts(sock_addr as *const libc::sockaddr, sock_len as u32) };

    addr.as_inet()
        .map(|v| v.into())
        .or(addr.as_inet6().map(|v| v.into()))
        .expect("neither ipv4 nor ipv6?")
}

pub struct StatelessPacket {
    packet: *mut picoquic_stateless_packet_t,
}

impl StatelessPacket {
    fn new(packet: *mut picoquic_stateless_packet_t) -> StatelessPacket {
        StatelessPacket {packet}
    }

    pub fn get_peer_addr(&self) -> SocketAddr {
        let addr = unsafe {
            mem::transmute::<&mut picoquic::sockaddr_storage, *mut picoquic::sockaddr>(
                &mut (*self.packet).addr_to,
            )
        };
        let socket_family = unsafe { (*self.packet).addr_to.ss_family };

        let socket_len = if socket_family as i32 == libc::AF_INET {
            mem::size_of::<libc::sockaddr_in>()
        } else {
            mem::size_of::<libc::sockaddr_in6>()
        };

        socket_addr_from_c(addr, socket_len as i32)
    }

    pub fn get_data(&self) -> &[u8] {
        unsafe {
            slice::from_raw_parts(mem::transmute(&(*self.packet).bytes), (*self.packet).length)
        }
    }
}

impl Drop for StatelessPacket {
    fn drop(&mut self) {
        unsafe {
            picoquic_delete_stateless_packet(self.packet);
        }
    }
}
