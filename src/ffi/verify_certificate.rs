use error::*;
use certificates::VerifyCertificate;
use ffi::QuicCtx;

use picoquic_sys::picoquic::{picoquic_cnx_t, ptls_iovec_t, verify_sign_cb_fn,
                             PTLS_ALERT_CERTIFICATE_EXPIRED, PTLS_ALERT_CERTIFICATE_REVOKED,
                             PTLS_ALERT_CERTIFICATE_UNKNOWN, PTLS_ERROR_NO_MEMORY,PTLS_ALERT_BAD_CERTIFICATE };

use std::os::raw::{c_int, c_void};
use std::slice;

use openssl::error::ErrorStack;
use openssl::x509::X509;
use openssl_sys::{X509_V_ERR_CERT_HAS_EXPIRED, X509_V_ERR_CERT_REVOKED, X509_V_ERR_OUT_OF_MEM};

pub fn setup_callback(quic: &QuicCtx, handler: Box<VerifyCertificate>) -> Result<(), Error> {
    Ok(())
}

unsafe extern "C" fn verify_sign_callback(
    ctx: *mut c_void,
    data: ptls_iovec_t,
    sign: ptls_iovec_t,
) -> c_int {
    0
}

unsafe extern "C" fn verify_certificate_callback(
    ctx: *mut c_void,
    cnx: *mut picoquic_cnx_t,
    certs: *mut ptls_iovec_t,
    num_certs: usize,
    verify_sign: *mut verify_sign_cb_fn,
    verify_sign_ctx: *mut *mut c_void,
) -> c_int {
    let mut handler = get_handler(ctx);

    if num_certs == 0 {
        return PTLS_ALERT_CERTIFICATE_UNKNOWN as i32;
    }

    let (cert, chain) = match extract_certificates(certs, num_certs) {
        Ok(res) => res,
        Err(_) => return PTLS_ALERT_BAD_CERTIFICATE as i32,
    };

    let pkey = match handler.verify(cert, chain) {
        Ok(key) => key,
        Err(e) => return ssl_error_to_error_code(e) as i32,
    };

    *verify_sign = Some(verify_sign_callback);
    *verify_sign_ctx = Box::into_raw(Box::new(pkey)) as *mut c_void;
    0
}

fn get_handler(ptr: *mut c_void) -> Box<Box<VerifyCertificate>> {
    unsafe { Box::from_raw(ptr as *mut Box<VerifyCertificate>) }
}

fn ssl_error_to_error_code(error: ErrorStack) -> u32 {
    if let Some(error) = error.errors().first() {
        match error.code() as i32 {
            X509_V_ERR_OUT_OF_MEM => PTLS_ERROR_NO_MEMORY,
            X509_V_ERR_CERT_REVOKED => PTLS_ALERT_CERTIFICATE_REVOKED,
            X509_V_ERR_CERT_HAS_EXPIRED => PTLS_ALERT_CERTIFICATE_EXPIRED,
            _ => PTLS_ALERT_CERTIFICATE_UNKNOWN,
        }
    } else {
        PTLS_ALERT_CERTIFICATE_UNKNOWN
    }
}

fn extract_certificates(
    certs: *mut ptls_iovec_t,
    num_certs: usize,
) -> Result<(X509, Vec<X509>), ErrorStack> {
    let certs = unsafe { slice::from_raw_parts_mut(certs, num_certs) };
    let cert = extract_certificate(certs[0])?;
    let mut chain = Vec::new();

    for i in 1..num_certs {
        chain.push(extract_certificate(certs[i])?);
    }

    Ok((cert, chain))
}

fn extract_certificate(cert: ptls_iovec_t) -> Result<X509, ErrorStack> {
    let data = unsafe { slice::from_raw_parts_mut(cert.base, cert.len) };
    X509::from_der(data)
}
