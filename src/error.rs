use std::ffi;
use std::fmt;

pub use failure::ResultExt;
use failure::{Backtrace, Context, Fail};

use bytes::BytesMut;

use futures;

use openssl;

#[derive(Debug)]
pub struct Error {
    inner: Context<ErrorKind>,
}

impl Fail for Error {
    fn cause(&self) -> Option<&Fail> {
        self.inner.cause()
    }

    fn backtrace(&self) -> Option<&Backtrace> {
        self.inner.backtrace()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Display::fmt(&self.inner, f)
    }
}

impl Error {
    pub fn kind(&self) -> &ErrorKind {
        &self.inner.get_context()
    }
}

impl From<ErrorKind> for Error {
    fn from(kind: ErrorKind) -> Error {
        Error {
            inner: Context::new(kind),
        }
    }
}

impl From<Context<ErrorKind>> for Error {
    fn from(inner: Context<ErrorKind>) -> Error {
        Error { inner: inner }
    }
}

impl From<futures::Canceled> for Error {
    fn from(_: futures::Canceled) -> Error {
        ErrorKind::InternalError.into()
    }
}

impl From<ffi::NulError> for Error {
    fn from(_: ffi::NulError) -> Error {
        ErrorKind::FFIError.into()
    }
}

impl From<openssl::error::ErrorStack> for Error {
    fn from(_: openssl::error::ErrorStack) -> Error {
        ErrorKind::OpenSSLError.into()
    }
}

#[derive(Clone, Eq, PartialEq, Debug, Fail)]
pub enum ErrorKind {
    #[fail(display = "A network error occurred.")]
    NetworkError,
    #[fail(display = "A ffi error occurred.")]
    FFIError,
    #[fail(display = "Could not allocate new memory.")]
    OutOfMemoryError,
    #[fail(display = "Disconnected.")]
    Disconnected,
    #[fail(display = "Unknown.")]
    Unknown,
    #[fail(display = "Send failed.")]
    SendError(BytesMut),
    #[fail(display = "An error occurred in the TLS handshake.")]
    TLSHandshakeError,
    #[fail(display = "An internal error occurred.")]
    InternalError,
    #[fail(display = "A string contains none unicode symbols.")]
    NoneUnicode,
    #[fail(display = "An OpenSSL error occurred.")]
    OpenSSLError,
}
