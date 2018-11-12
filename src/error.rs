use std::{ffi, fmt, io};

pub use failure::ResultExt;
use failure::{self, Backtrace, Context, Fail};

use bytes::Bytes;

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
        Error { inner }
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

impl From<failure::Error> for Error {
    fn from(e: failure::Error) -> Error {
        ErrorKind::Custom(e).into()
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        ErrorKind::Io(e).into()
    }
}

#[derive(Debug, Fail)]
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
    SendError(Bytes),
    #[fail(display = "An error occurred in the TLS handshake.")]
    TLSHandshakeError,
    #[fail(display = "An internal error occurred.")]
    InternalError,
    #[fail(display = "A string contains none unicode symbols.")]
    NoneUnicode,
    #[fail(display = "An OpenSSL error occurred.")]
    OpenSSLError,
    #[fail(display = "Error {}", _0)]
    Custom(failure::Error),
    #[fail(display = "IO error {}", _0)]
    Io(io::Error),
}

//FIXME: Remove when upstream provides a better bail macro
macro_rules! bail {
    ($e:expr) => {
        return Err(::failure::err_msg::<&'static str>($e).into());
    };
    ($fmt:expr, $($arg:tt)+) => {
        return Err(::failure::err_msg::<String>(format!($fmt, $($arg)+)).into());
    };
}

/// A function that returns the an error.
pub trait ErrorFn: Send + 'static + Sync + Fn() -> Error {}
impl<T: Fn() -> Error + Send + 'static + Sync> ErrorFn for T {}
