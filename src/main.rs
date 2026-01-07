use crate::clipboard::ClipboardListener;
use crate::error::UniClipError::*;
use crate::options::*;
use crate::proto::Connection;
use crate::{options::set_options, proto::Command};

use arboard::Error::*;

use tokio::signal;
use tokio::sync::{broadcast, mpsc};
use tokio::time::{Duration, sleep};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use std::env;

extern crate pretty_env_logger;

#[macro_use]
extern crate log;

mod clipboard;
mod error;
mod options;
mod proto;
mod timestamp;

#[tokio::main]
async fn main() {
    if env::var("RUST_LOG").is_err() {
        // Safety: Only set envvar once, and only if it is not found.
        unsafe {
            env::set_var("RUST_LOG", "info");
        }
    }

    pretty_env_logger::init();

    let (server_mode, connection_string) = set_options();

    let (broadcast_tx, broadcast_rx) = broadcast::channel::<Command>(16);
    let (mpsc_tx, mpsc_rx) = mpsc::channel::<Command>(16);

    let tracker = TaskTracker::new();
    let main_cancel_token = CancellationToken::new();

    let listener = loop {
        let listener = ClipboardListener::new();
        if let Err(e) = listener {
            match e {
                ClipboardError(ContentNotAvailable)
                | ClipboardError(ClipboardOccupied)
                | ClipboardError(ConversionFailure) => {
                    // Retry after timeout.
                    sleep(Duration::from_millis(get_option(CLIPBOARD_POLL_FREQ_MS))).await;
                    continue;
                }
                _ => {
                    e.panic(&"LOCAL".to_string(), &main_cancel_token);
                    return;
                }
            }
        };
        break listener.unwrap();
    };

    info!("Starting clipboard listener.");
    listener.listen(mpsc_rx, &tracker, broadcast_tx, main_cancel_token.clone());

    if server_mode {
        info!("Spawning server.");
        // spawn_serve will close the tracker after it finishes.
        Connection::spawn_serve(
            connection_string,
            &tracker,
            main_cancel_token.clone(),
            mpsc_tx,
            broadcast_rx,
        );
    } else {
        // Client would not close the tracker.
        info!("Connecting to {}.", connection_string);
        Connection::spawn_connect(
            connection_string,
            &tracker,
            main_cancel_token.clone(),
            mpsc_tx,
            broadcast_rx,
        )
        .await;
        tracker.close();
    }

    tokio::select! {
        _ = signal::ctrl_c() => {
            warn!("CTRL-C Received. Exiting.");
            main_cancel_token.cancel();
            tracker.wait().await;
        }
        _ = tracker.wait() => {}
    };
}
