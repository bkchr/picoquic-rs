use super::{connection::ConnectionIter, stateless_packet::StatelessPacketIter, Pointer};
use config::{Config, FileFormat};
use error::*;
use ffi::verify_certificate;

use picoquic_sys::picoquic::{
    self, picoquic_create, picoquic_current_time, picoquic_free, picoquic_get_next_wake_delay,
    picoquic_incoming_packet, picoquic_quic_t, picoquic_set_client_authentication,
    picoquic_set_tls_certificate_chain, picoquic_set_tls_key, picoquic_set_tls_root_certificates,
    picoquic_stream_data_cb_fn, ptls_iovec_t,
};

use std::{
    ffi::CString,
    mem,
    net::SocketAddr,
    os::raw::{c_char, c_void},
    path::PathBuf,
    ptr,
    time::{Duration, Instant},
};

use socket2::SockAddr;

use libc;

use openssl::pkey::PKey;
use openssl::x509::X509;

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

fn c_str_or_null(string: &Option<CString>) -> *const c_char {
    string
        .as_ref()
        .map(|v| v.as_ptr())
        .unwrap_or_else(ptr::null)
}

pub struct QuicCtx {
    quic: Pointer<picoquic_quic_t>,
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

        let cert_filename = create_cstring(config.certificate_chain_filename)?;
        let key_filename = create_cstring(config.private_key_filename)?;
        let root_cert_filename = create_cstring(config.root_certificate_filename)?;

        let reset_seed = config
            .reset_seed
            .as_mut()
            .map(|v| v.as_mut_ptr())
            .unwrap_or_else(ptr::null_mut);

        let quic = unsafe {
            picoquic_create(
                connection_buckets,
                c_str_or_null(&cert_filename),
                c_str_or_null(&key_filename),
                c_str_or_null(&root_cert_filename),
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

        let mut quic = QuicCtx {
            quic: Pointer(quic),
            max_delay: Duration::from_secs(10),
        };

        if config.client_authentication {
            unsafe {
                picoquic_set_client_authentication(quic.as_ptr(), 1);
            }
        }

        if let Some((format, chain)) = config.certificate_chain {
            quic.set_tls_certificate_chain(chain, format)?;
        }

        if let Some((format, key)) = config.private_key {
            quic.set_tls_private_key(key, format)?;
        }

        if let Some((format, certs)) = config.root_certificates {
            quic.set_tls_root_certificates(certs, format)?;
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
            quic: Pointer(ptr::null_mut()),
            max_delay: Duration::from_secs(10),
        }
    }

    pub fn as_ptr(&self) -> *mut picoquic_quic_t {
        *self.quic
    }

    /// Returns an iterator over all connections, ordered by their next wake up time.
    pub fn ordered_connection_iter(&self, current_time: u64) -> ConnectionIter {
        ConnectionIter::new(*self.quic, current_time)
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
                *self.quic,
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

    pub fn stateless_packet_iter(&self) -> StatelessPacketIter {
        StatelessPacketIter::new(*self.quic)
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
        let wake_up = unsafe { picoquic_get_next_wake_delay(*self.quic, current_time, max_delay) };

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

    /// Sets the tls certificate chain.
    fn set_tls_certificate_chain(
        &mut self,
        chain: Vec<Vec<u8>>,
        format: FileFormat,
    ) -> Result<(), Error> {
        let (certs_ptr, len) = make_certs_iovec(chain, format)?;

        unsafe {
            picoquic_set_tls_certificate_chain(self.as_ptr(), certs_ptr, len);
        }

        Ok(())
    }

    /// Sets the tls root certificates.
    fn set_tls_root_certificates(
        &mut self,
        certs: Vec<Vec<u8>>,
        format: FileFormat,
    ) -> Result<(), Error> {
        let (certs_ptr, len) = make_certs_iovec(certs, format)?;

        let res = unsafe { picoquic_set_tls_root_certificates(self.as_ptr(), certs_ptr, len) };

        if res == -1 {
            bail!("Error at loading a root certificate")
        } else if res == -2 {
            bail!("Error at adding a root certificate, maybe a duplicate?")
        } else {
            Ok(())
        }
    }

    /// Sets the tls private key.
    fn set_tls_private_key(&mut self, key: Vec<u8>, format: FileFormat) -> Result<(), Error> {
        let mut key = match format {
            FileFormat::DER => key,
            FileFormat::PEM => PKey::private_key_from_pem(&key)?.private_key_to_der()?,
        };

        let len = key.len();
        let key_ptr = key.as_mut_ptr();

        unsafe {
            let res = picoquic_set_tls_key(self.as_ptr(), key_ptr, len);

            if res == 0 {
                Ok(())
            } else {
                Err(ErrorKind::Unknown.into())
            }
        }
    }
}

impl Drop for QuicCtx {
    fn drop(&mut self) {
        unsafe {
            picoquic_free(*self.quic);
        }
    }
}

fn make_certs_iovec(
    certs: Vec<Vec<u8>>,
    format: FileFormat,
) -> Result<(*mut ptls_iovec_t, usize), Error> {
    let certs = match format {
        FileFormat::DER => certs,
        FileFormat::PEM => {
            let mut res = Vec::with_capacity(certs.len());
            for cert in certs {
                res.push(X509::from_pem(&cert)?.to_der()?);
            }
            res
        }
    };

    let mut certs = certs
        .into_iter()
        .map(|mut cert| {
            let len = cert.len();
            let base = cert.as_mut_ptr();
            mem::forget(cert);

            ptls_iovec_t { len, base }
        })
        .collect::<Vec<_>>();

    let len = certs.len();
    let certs_ptr = certs.as_mut_ptr();
    mem::forget(certs);

    Ok((certs_ptr, len))
}

pub fn socket_addr_from_sockaddr(sock_addr: *mut picoquic::sockaddr, sock_len: i32) -> SocketAddr {
    let addr =
        unsafe { SockAddr::from_raw_parts(sock_addr as *const libc::sockaddr, sock_len as u32) };

    addr.as_inet()
        .map(|v| v.into())
        .or_else(|| addr.as_inet6().map(|v| v.into()))
        .expect("neither ipv4 nor ipv6?")
}

pub fn socket_addr_from_sockaddr_storage(
    sock_addr_storge: *const picoquic::sockaddr_storage,
    sock_len: i32,
) -> SocketAddr {
    socket_addr_from_sockaddr(sock_addr_storge as *mut picoquic::sockaddr, sock_len)
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
        self.as_secs() * 1_000_000 + u64::from(self.subsec_nanos()) / 1000
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
