use super::quic_ctx::socket_addr_from_c;
use picoquic_sys::picoquic::{self, picoquic_delete_stateless_packet,
                             picoquic_dequeue_stateless_packet, picoquic_quic_t,
                             picoquic_stateless_packet_t};

use std::iter::Iterator;
use std::mem;
use std::slice;
use std::net::SocketAddr;
use std::marker::PhantomData;

use libc;

pub struct StatelessPacket {
    packet: *mut picoquic_stateless_packet_t,
}

impl StatelessPacket {
    fn new(packet: *mut picoquic_stateless_packet_t) -> StatelessPacket {
        StatelessPacket { packet }
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

pub struct StatelessPacketIter<'a> {
    quic: *mut picoquic_quic_t,
    _marker: PhantomData<&'a i32>,
}

impl<'a> StatelessPacketIter<'a> {
    pub fn new(quic: *mut picoquic_quic_t) -> StatelessPacketIter<'a> {
        StatelessPacketIter { quic, _marker: Default::default() }
    }
}

impl<'a> Iterator for StatelessPacketIter<'a> {
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
