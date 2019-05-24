use crate::error::*;
use crate::ffi::{Connection, QuicCtx};
use crate::verify_certificate::VerifyCertificate;

use picoquic_sys::picoquic::{
    picoquic_cnx_t, picoquic_set_verify_certificate_callback, picoquic_verify_sign_cb_fn,
    ptls_iovec_t, PTLS_ALERT_BAD_CERTIFICATE, PTLS_ALERT_CERTIFICATE_EXPIRED,
    PTLS_ALERT_CERTIFICATE_REVOKED, PTLS_ALERT_CERTIFICATE_UNKNOWN, PTLS_ALERT_DECRYPT_ERROR,
    PTLS_ERROR_LIBRARY, PTLS_ERROR_NO_MEMORY,
};

use std::mem;
use std::os::raw::{c_int, c_void};
use std::slice;

use openssl::error::ErrorStack;
use openssl::hash::MessageDigest;
use openssl::pkey::{Id, PKey, Public};
use openssl::rsa::Padding;
use openssl::sign::{RsaPssSaltlen, Verifier};
use openssl::stack::Stack;
use openssl::x509::X509;

use openssl_sys::{X509_V_ERR_CERT_HAS_EXPIRED, X509_V_ERR_CERT_REVOKED, X509_V_ERR_OUT_OF_MEM};

pub type PubKey = PKey<Public>;

/// Sets up the verify certificate callback in picoquic
pub fn setup_callback(quic: &QuicCtx, handler: Box<dyn VerifyCertificate>) -> Result<(), Error> {
    let result;
    unsafe {
        let ctx = Box::into_raw(Box::new(handler));

        result = picoquic_set_verify_certificate_callback(
            quic.as_ptr(),
            Some(verify_certificate_callback),
            ctx as *mut c_void,
            Some(free_ctx),
        );
    }

    if result != 0 {
        Err(ErrorKind::OutOfMemoryError.into())
    } else {
        Ok(())
    }
}

/// Will be called by picoquic to free the handler context
unsafe extern "C" fn free_ctx(ctx: *mut c_void) {
    let _ = get_handler(ctx);
}

/// Will be called by picoquic to verify the signed data
unsafe extern "C" fn verify_sign_callback(
    ctx: *mut c_void,
    data: ptls_iovec_t,
    sign: ptls_iovec_t,
) -> c_int {
    let pkey = get_pkey(ctx);
    let data = slice::from_raw_parts(data.base, data.len);
    let sign = slice::from_raw_parts(sign.base, sign.len);

    if data.is_empty() || sign.is_empty() {
        return 0;
    }

    let mut verifier = match Verifier::new(MessageDigest::sha256(), &pkey) {
        Ok(verifier) => verifier,
        Err(_) => return PTLS_ERROR_LIBRARY as i32,
    };

    if pkey.id() == Id::RSA {
        if verifier.set_rsa_padding(Padding::PKCS1_PSS).is_err() {
            return PTLS_ERROR_LIBRARY as i32;
        }

        if verifier
            .set_rsa_pss_saltlen(RsaPssSaltlen::DIGEST_LENGTH)
            .is_err()
        {
            return PTLS_ERROR_LIBRARY as i32;
        }

        if verifier.set_rsa_mgf1_md(MessageDigest::sha256()).is_err() {
            return PTLS_ERROR_LIBRARY as i32;
        }
    }

    if verifier.update(data).is_err() {
        return PTLS_ERROR_LIBRARY as i32;
    }

    if verifier.verify(sign).unwrap_or(false) {
        0
    } else {
        PTLS_ALERT_DECRYPT_ERROR as i32
    }
}

fn get_pkey(ptr: *mut c_void) -> Box<PubKey> {
    unsafe { Box::from_raw(ptr as *mut PubKey) }
}

/// The main verify certificate callback
unsafe extern "C" fn verify_certificate_callback(
    ctx: *mut c_void,
    cnx: *mut picoquic_cnx_t,
    certs: *mut ptls_iovec_t,
    num_certs: usize,
    verify_sign: *mut picoquic_verify_sign_cb_fn,
    verify_sign_ctx: *mut *mut c_void,
) -> c_int {
    let mut handler = get_handler(ctx);

    let result = verify_certificate_callback_impl(
        &mut **handler,
        cnx,
        certs,
        num_certs,
        verify_sign,
        verify_sign_ctx,
    );

    mem::forget(handler);

    result as i32
}

fn verify_certificate_callback_impl(
    handler: &mut VerifyCertificate,
    cnx: *mut picoquic_cnx_t,
    certs: *mut ptls_iovec_t,
    num_certs: usize,
    verify_sign: *mut picoquic_verify_sign_cb_fn,
    verify_sign_ctx: *mut *mut c_void,
) -> u32 {
    if num_certs == 0 {
        return PTLS_ALERT_CERTIFICATE_UNKNOWN;
    }

    let (cert, chain) = match extract_certificates(certs, num_certs) {
        Ok(res) => res,
        Err(_) => return PTLS_ALERT_BAD_CERTIFICATE,
    };

    let cnx = Connection::from(cnx);

    let id = cnx.local_id();

    match handler.verify(id, cnx.con_type(), &cert, &chain) {
        Ok(true) => {}
        Ok(false) => {
            return PTLS_ALERT_CERTIFICATE_UNKNOWN;
        }
        Err(e) => return ssl_error_to_error_code(&e),
    };

    // Extract the public key, as we need this public key to verify the signed data in
    // `verify_sign_callback`.
    let pkey = match cert.public_key() {
        Ok(pkey) => pkey,
        Err(e) => return ssl_error_to_error_code(&e),
    };

    unsafe {
        *verify_sign = Some(verify_sign_callback);
        *verify_sign_ctx = Box::into_raw(Box::new(pkey)) as *mut c_void;
    }

    0
}

fn get_handler(ptr: *mut c_void) -> Box<Box<dyn VerifyCertificate>> {
    unsafe { Box::from_raw(ptr as *mut Box<dyn VerifyCertificate>) }
}

/// Converts a openssl error to a picotls error
fn ssl_error_to_error_code(error: &ErrorStack) -> u32 {
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
) -> Result<(X509, Stack<X509>), ErrorStack> {
    let certs = unsafe { slice::from_raw_parts_mut(certs, num_certs) };
    let cert = extract_certificate(certs[0])?;
    let mut chain = Stack::new()?;

    for cert in certs.iter().skip(1) {
        chain.push(extract_certificate(*cert)?)?;
    }

    Ok((cert, chain))
}

fn extract_certificate(cert: ptls_iovec_t) -> Result<X509, ErrorStack> {
    let data = unsafe { slice::from_raw_parts_mut(cert.base, cert.len) };
    X509::from_der(data)
}
