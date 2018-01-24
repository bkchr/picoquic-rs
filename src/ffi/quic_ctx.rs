use error::*;
use super::Connection;
use config::Config;

use picoquic_sys::picoquic::{picoquic_cnx_t, picoquic_create, picoquic_free,
                             picoquic_get_first_cnx, picoquic_get_next_cnx, picoquic_quic_t,
                             picoquic_stream_data_cb_fn};

use std::iter::Iterator;
use std::os::raw::c_void;
use std::ffi::CString;
use std::ptr;

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
