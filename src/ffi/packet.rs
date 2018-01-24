use error::*;
use ffi::Connection;

use picoquic_sys::picoquic::{picoquic_create_packet, picoquic_packet, picoquic_prepare_packet,
                             PICOQUIC_ERROR_DISCONNECTED};

use tokio_core::net::UdpSocket;

use std::net::SocketAddr;

use libc;

pub struct Packet<'a> {
    buffer: &'a mut [u8],
    packet: *mut picoquic_packet,
    len: usize,
}

impl<'a> Packet<'a> {
    pub fn create(
        buffer: &'a mut [u8],
        con: &Connection,
        current_time: u64,
    ) -> Result<Packet<'a>, Error> {
        let packet = unsafe { picoquic_create_packet() };

        if packet.is_null() {
            return Err(ErrorKind::OutOfMemoryError.into());
        } else {
            let mut packet = Packet {
                buffer,
                packet,
                len: 0,
            };

            packet.len = packet.prepare(current_time, con)?;

            Ok(packet)
        }
    }

    fn prepare(&mut self, current_time: u64, con: &Connection) -> Result<usize, Error> {
        let mut send_len = 0;
        let ret = unsafe {
            picoquic_prepare_packet(
                con.as_ptr(),
                self.packet,
                current_time,
                self.buffer.as_mut_ptr(),
                self.buffer.len(),
                &mut send_len,
            )
        };

        if ret == PICOQUIC_ERROR_DISCONNECTED as i32 {
            Err(ErrorKind::Disconnected.into())
        } else if ret == 0 {
            Ok(send_len)
        } else {
            Err(ErrorKind::Unknown.into())
        }
    }

    pub fn send(&self, socket: &mut UdpSocket, peer_addr: SocketAddr) -> Result<(), Error> {
        // check that the packet contains any data
        if unsafe { (*self.packet).length > 0 } {
            socket
                .send_to(&self.buffer[..self.len], &peer_addr)
                .map(|_| ())
                .context(ErrorKind::NetworkError)?;
        }

        Ok(())
    }
}

impl<'a> Drop for Packet<'a> {
    fn drop(&mut self) {
        unsafe {
            libc::free(self.packet as *mut libc::c_void);
        }
    }
}
