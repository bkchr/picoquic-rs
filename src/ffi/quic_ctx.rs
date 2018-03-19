use error::*;
use super::connection::ConnectionIter;
use super::stateless_packet::StatelessPacketIter;
use config::Config;
use ffi::verify_certificate;

use picoquic_sys::picoquic::{self, picoquic_create, picoquic_current_time, picoquic_free,
                             picoquic_get_next_wake_delay, picoquic_incoming_packet,
                             picoquic_quic_t, picoquic_set_client_authentication,
                             picoquic_stream_data_cb_fn};

use std::os::raw::c_void;
use std::ffi::CString;
use std::ptr;
use std::net::SocketAddr;
use std::time::{Duration, Instant};
use std::path::PathBuf;

use socket2::SockAddr;

use libc;

pub struct QuicCtx {
    quic: *mut picoquic_quic_t,
    max_delay: Duration,
}

impl QuicCtx {
    pub fn new(
        mut config: Config,
        default_ctx: *mut c_void,
        default_callback: picoquic_stream_data_cb_fn,
    ) -> Result<QuicCtx, Error> {
        // The number of buckets that picoquic will allocate for connections
        // The buckets itself are a linked list
        let connection_buckets = 16;

        fn create_cstring(path: Option<PathBuf>) -> Result<Option<CString>, Error> {
            match path {
                Some(p) => {
                    let string = match p.into_os_string().into_string() {
                        Ok(string) => string,
                        Err(_) => return Err(ErrorKind::NoneUnicode.into()),
                    };
                    Ok(Some(CString::new(string)?))
                }
                None => Ok(None),
            }
        }

        let cert_filename = create_cstring(config.cert_chain_filename)?;
        let key_filename = create_cstring(config.key_filename)?;

        let reset_seed = config
            .reset_seed
            .as_mut()
            .map(|v| v.as_mut_ptr())
            .unwrap_or_else(|| ptr::null_mut());

        let quic = unsafe {
            picoquic_create(
                connection_buckets,
                cert_filename
                    .as_ref()
                    .map(|v| v.as_ptr())
                    .unwrap_or_else(|| ptr::null_mut()),
                key_filename
                    .as_ref()
                    .map(|v| v.as_ptr())
                    .unwrap_or_else(|| ptr::null_mut()),
                ptr::null(),
                default_callback,
                default_ctx,
                None,
                ptr::null_mut(),
                reset_seed,
                picoquic_current_time(),
                ptr::null_mut(),
                ptr::null(),
                ptr::null(),
                0,
            )
        };
        assert!(!quic.is_null());

        let quic = QuicCtx {
            quic,
            max_delay: Duration::from_secs(10),
        };

        if config.client_authentication {
            unsafe {
                picoquic_set_client_authentication(quic.as_ptr(), 1);
            }
        }

        if let Some(handler) = config.verify_certificate_handler.take() {
            verify_certificate::setup_callback(&quic, handler)?;
        }

        Ok(quic)
    }

    /// Creates a dummy instance, that uses a `NULL` pointer for the quic context.
    /// This function must only be used in tests!
    #[doc(hidden)]
    #[cfg(test)]
    pub fn dummy() -> QuicCtx {
        QuicCtx {
            quic: ptr::null_mut(),
            max_delay: Duration::from_secs(10),
        }
    }

    pub fn as_ptr(&self) -> *mut picoquic_quic_t {
        self.quic
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

        let ret = unsafe {
            picoquic_incoming_packet(
                self.quic,
                buf.as_mut_ptr(),
                buf.len() as u32,
                addr_from.as_ptr() as *mut picoquic::sockaddr,
                addr_to.as_ptr() as *mut picoquic::sockaddr,
                // as long as we only support one udp socket, we don't need to change this index
                0,
                current_time,
            )
        };

        if ret != 0 {
            error!("`picoquic_incoming_packet` returned: {}", ret);
        }
    }

    pub fn stateless_packet_iter<'a>(&'a self) -> StatelessPacketIter<'a> {
        StatelessPacketIter::new(self.quic)
    }

    /// Returns the next time point at which Picoquic needs to get called again. However, it is
    /// possible to call Picoquic before, e.g. when new data arrives or the application wants to
    /// send new data. The time point is absolute.
    ///
    /// # Returns
    /// Some(_) is the next latest time Picoquic wants to get called again. None intends that
    /// Picoquic wants to get called again instantly.
    pub fn get_next_wake_up_time(&self, current_time: u64) -> Option<Instant> {
        let max_delay = self.max_delay.as_micro_seconds() as i64;
        let wake_up = unsafe { picoquic_get_next_wake_delay(self.quic, current_time, max_delay) };

        if wake_up == 0 {
            None
        } else {
            // TODO: maybe we need to use current_time here.
            Some(Instant::now() + Duration::from_micro_seconds(wake_up as u64))
        }
    }

    /// Returns the current time in micro seconds for Picoquic.
    pub fn get_current_time(&self) -> u64 {
        unsafe { picoquic_current_time() }
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

pub trait MicroSeconds {
    fn from_micro_seconds(micros: u64) -> Self;
    fn as_micro_seconds(&self) -> u64;
}

impl MicroSeconds for Duration {
    fn from_micro_seconds(micros: u64) -> Duration {
        let secs = micros / 1_000_000;
        let nanos = micros % 1_000_000 * 1000;

        Duration::new(secs, nanos as u32)
    }

    fn as_micro_seconds(&self) -> u64 {
        self.as_secs() * 1_000_000 + self.subsec_nanos() as u64 / 1000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_micro_seconds() {
        assert_eq!(
            Duration::from_secs(1),
            Duration::from_micro_seconds(1_000_000)
        );
        assert_eq!(Duration::new(0, 1000), Duration::from_micro_seconds(1));
        assert_eq!(
            Duration::new(1, 5000),
            Duration::from_micro_seconds(1_000_005)
        );
        assert_eq!(Duration::new(0, 500000), Duration::from_micro_seconds(500));
    }

    #[test]
    fn as_micro_seconds() {
        assert_eq!(Duration::from_secs(1).as_micro_seconds(), 1_000_000);
        assert_eq!(Duration::new(0, 1000).as_micro_seconds(), 1);
        assert_eq!(Duration::new(1, 5000).as_micro_seconds(), 1_000_005);
        assert_eq!(Duration::new(0, 500000).as_micro_seconds(), 500);
    }
}
