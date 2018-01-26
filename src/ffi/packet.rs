use error::*;
use ffi::Connection;

use picoquic_sys::picoquic::{picoquic_create_packet, picoquic_packet, picoquic_prepare_packet,
                             PICOQUIC_ERROR_DISCONNECTED};

use libc;

pub struct Packet<'a> {
    buffer: &'a mut [u8],
    packet: *mut picoquic_packet,
}

impl<'a> Packet<'a> {
    pub fn create(buffer: &'a mut [u8]) -> Result<Packet<'a>, Error> {
        let packet = unsafe { picoquic_create_packet() };

        if packet.is_null() {
            return Err(ErrorKind::OutOfMemoryError.into());
        } else {
            Ok(Packet { buffer, packet })
        }
    }

    pub fn prepare(&mut self, current_time: u64, con: &Connection) -> Result<usize, Error> {
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

    pub fn contains_data(&self) -> bool {
        unsafe { (*self.packet).length > 0 }
    }
}

impl<'a> Drop for Packet<'a> {
    fn drop(&mut self) {
        // TODO: really shitty :( I hope there is no valid state where the packet contains no data!
        if !self.contains_data() {
            unsafe {
                libc::free(self.packet as *mut libc::c_void);
            }
        }
    }
}
