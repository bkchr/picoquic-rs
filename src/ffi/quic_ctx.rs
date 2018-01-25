use error::*;
use super::connection::ConnectionIter;
use super::stateless_packet::StatelessPacketIter;
use config::Config;

use picoquic_sys::picoquic::{self, picoquic_create,
                             picoquic_free, 
                             picoquic_incoming_packet, picoquic_quic_t,
                             picoquic_stream_data_cb_fn};

use std::os::raw::c_void;
use std::ffi::CString;
use std::ptr;
use std::net::SocketAddr;

use socket2::SockAddr;

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

    pub fn stateless_packet_iter(&self) -> StatelessPacketIter {
        // TODO, ensure that the iterator lives not longer than the context(some lifetime magic)
        StatelessPacketIter::new(self.quic)
    }
    
}

impl Drop for QuicCtx {
    fn drop(&mut self) {
        unsafe {
            picoquic_free(self.quic);
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

