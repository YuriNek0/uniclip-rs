use std::convert::{AsMut, AsRef};
use std::fmt;

use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub enum UniClipError {
    DisconnectedError,
    TimeoutError,
    SystemClockError,
    MessageTooLargeError,
    ConnectionError(String),
    PacketParseError(String),
    ClientError(String),
    ChannelError(String),
    ClipboardError(arboard::Error),
    IOError(String),
    UnknownError(String),
}

impl AsRef<UniClipError> for UniClipError {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl AsMut<UniClipError> for UniClipError {
    fn as_mut(&mut self) -> &mut Self {
        self
    }
}

impl fmt::Display for UniClipError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            UniClipError::DisconnectedError => write!(f, "Client disconnected."),
            UniClipError::TimeoutError => write!(f, "Timed out while trying to reach the client."),
            UniClipError::SystemClockError => {
                write!(f, "Could not use System Clock. Are these clocks synced?")
            }
            UniClipError::MessageTooLargeError => {
                write!(f, "Trying to send a message that is too large.")
            }
            UniClipError::ConnectionError(msg) => {
                write!(f, "Broken connection. Details: {}", msg)
            }
            UniClipError::PacketParseError(msg) => {
                write!(f, "Failed to parse the packet. Details: {}", msg)
            }
            UniClipError::ClientError(msg) => {
                write!(f, "Error received from the client: {}", msg)
            }
            UniClipError::ChannelError(msg) => {
                write!(f, "Failed to send/recv from a channel: {}", msg)
            }
            UniClipError::ClipboardError(e) => {
                write!(
                    f,
                    "System clipboard is not available. Details: {}",
                    e.to_string()
                )
            }
            UniClipError::IOError(msg) => write!(f, "IOError occured! Details: {}", msg),
            UniClipError::UnknownError(msg) => {
                write!(f, "An unknown error occured! Details: {}", msg)
            }
        }
    }
}

impl UniClipError {
    fn get_type(&self) -> &str {
        match self {
            UniClipError::DisconnectedError => "DisconnectedError",
            UniClipError::TimeoutError => "TimeoutError",
            UniClipError::SystemClockError => "SystemClockError",
            UniClipError::MessageTooLargeError => "MessageTooLargeError",
            UniClipError::ConnectionError(_) => "ConnectionError",
            UniClipError::PacketParseError(_) => "PacketParseError",
            UniClipError::ClientError(_) => "ClientError",
            UniClipError::ChannelError(_) => "ChannelError",
            UniClipError::ClipboardError(_) => "ClipboardError",
            UniClipError::IOError(_) => "IOError",
            UniClipError::UnknownError(_) => "UnknownError",
        }
    }
    pub fn report(&self, source: &String) {
        warn!("[{}] {}: {}\n", source, self.get_type(), self.to_string());
    }
    pub fn panic(self, source: &String, token: &CancellationToken) {
        error!("[{}] {}: {}\n", source, self.get_type(), self.to_string());
        token.cancel();
    }
}
