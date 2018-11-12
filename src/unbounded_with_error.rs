use error::*;

use futures::{
    sync::mpsc::{self, unbounded, UnboundedReceiver, UnboundedSender},
    Poll, Sink, StartSend, Stream,
};

use std::{fmt, sync::Arc};

use parking_lot::RwLock;

/// A state for storing a shared error.
#[derive(Clone)]
struct ErrorState {
    inner: Arc<RwLock<Option<Box<dyn ErrorFn<Output = Error>>>>>,
}

impl fmt::Debug for ErrorState {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "ErrorState")
    }
}

impl ErrorState {
    fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(None)),
        }
    }
    /// Return the stored error.
    pub fn error(&self) -> Option<Error> {
        self.inner.read().as_ref().map(|v| v())
    }

    /// Set the stored error function.
    pub fn set_error<T: ErrorFn>(&mut self, err_fn: T) {
        *self.inner.write() = Some(Box::new(err_fn));
    }
}

/// Error used by `Sender` to distinguish between an error set for the channel and
/// a normal `futures::SendError`.
pub enum SendError<T> {
    Channel(Error, T),
    Normal(T),
}

impl<T> SendError<T> {
    pub fn into_error<F: Fn(T) -> Error>(self, map_err: F) -> Error {
        match self {
            SendError::Channel(err, _) => err,
            SendError::Normal(data) => map_err(data),
        }
    }
}

/// A sender with an associated error that can be set by the receiver side.
#[derive(Debug)]
pub struct Sender<T> {
    sender: UnboundedSender<T>,
    error_state: ErrorState,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
            error_state: self.error_state.clone(),
        }
    }
}

impl<T> Sender<T> {
    fn new(sender: UnboundedSender<T>, error_state: ErrorState) -> Self {
        Self {
            sender,
            error_state,
        }
    }

    fn check_for_error(&mut self) -> Option<Error> {
        // If `sender` is closed, check if we got an error to propagate
        if self.sender.is_closed() {
            self.error_state.error()
        } else {
            None
        }
    }

    pub fn unbounded_send(&mut self, data: T) -> Result<(), SendError<T>> {
        self.sender
            .unbounded_send(data)
            .map_err(|e| self.map_err(e))
    }

    fn map_err(&mut self, e: mpsc::SendError<T>) -> SendError<T> {
        if let Some(err) = self.check_for_error() {
            SendError::Channel(err, e.into_inner())
        } else {
            SendError::Normal(e.into_inner())
        }
    }
}

impl<T> Sink for Sender<T> {
    type SinkItem = T;
    type SinkError = SendError<T>;

    fn start_send(&mut self, item: Self::SinkItem) -> StartSend<Self::SinkItem, Self::SinkError> {
        self.sender.start_send(item).map_err(|e| self.map_err(e))
    }

    fn poll_complete(&mut self) -> Poll<(), Self::SinkError> {
        self.sender.poll_complete().map_err(|e| self.map_err(e))
    }
}

/// A receiver with the ability to propagate an error to the sender.
pub struct Receiver<T> {
    receiver: UnboundedReceiver<T>,
    error_state: ErrorState,
}

impl<T> Receiver<T> {
    fn new(receiver: UnboundedReceiver<T>, error_state: ErrorState) -> Self {
        Self {
            receiver,
            error_state,
        }
    }

    /// Channel the given error to the `Sender` side.
    /// This will only work once, further calls of this function will just discard the error.
    pub fn propagate_error<F: ErrorFn + 'static>(&mut self, err: F) {
        self.error_state.set_error(err)
    }

    pub fn close(&mut self) {
        self.receiver.close();
    }
}

impl<T> Stream for Receiver<T> {
    type Item = T;
    type Error = Error;

    fn poll(&mut self) -> Poll<Option<Self::Item>, Self::Error> {
        self.receiver.poll().map_err(|_| ErrorKind::Unknown.into())
    }
}

pub fn unbounded_with_error<T>() -> (Sender<T>, Receiver<T>) {
    let (sender, receiver) = unbounded();
    let error_state = ErrorState::new();

    (
        Sender::new(sender, error_state.clone()),
        Receiver::new(receiver, error_state),
    )
}
