use arboard::Error::*;
use arboard::{Clipboard as ArBoard, ImageData};
use image::DynamicImage;
use image::codecs::png::PngDecoder;
use image::{ImageDecoder as _, ImageEncoder as _};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::error::UniClipError::{self, *};
use crate::options::get_option;
use crate::options::*;
use crate::proto::Command;
use crate::timestamp::get_utc_now;

#[cfg(target_os = "macos")]
use arboard::SetExtApple;
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "android", target_os = "emscripten"))
))]
use arboard::SetExtLinux;
#[cfg(windows)]
use arboard::SetExtWindows;

#[derive(Hash, Clone)]
enum ClipboardState {
    Text(String),
    Image(Vec<u8>),
}

#[derive(Clone)]
struct ClipboardContext {
    hash: u64,
    state: ClipboardState,
    stamp: u64,
}

struct Clipboard {
    ctx: ClipboardContext,
    backend: ArBoard,
}

// Modified from
// https://github.com/1Password/arboard/blob/223f4efc7a976ce403d6b74c0333833e092ed7b0/src/platform/linux/mod.rs
fn encode_as_png(image: &ImageData) -> Result<Vec<u8>, UniClipError> {
    if image.bytes.is_empty() || image.width == 0 || image.height == 0 {
        return Err(ClipboardError(ConversionFailure));
    }

    let mut png_bytes = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png_bytes);
    encoder
        .write_image(
            image.bytes.as_ref(),
            image.width as u32,
            image.height as u32,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|_| ClipboardError(ConversionFailure))?;

    Ok(png_bytes)
}

// Modified from
// https://github.com/1Password/arboard/blob/223f4efc7a976ce403d6b74c0333833e092ed7b0/src/platform/windows.rs
fn read_png(data: &[u8]) -> Result<ImageData<'static>, UniClipError> {
    let decoder = PngDecoder::new(std::io::Cursor::new(data))
        .map_err(|_| ClipboardError(ConversionFailure))?;
    let (width, height) = decoder.dimensions();

    let bytes = DynamicImage::from_decoder(decoder)
        .map_err(|_| ClipboardError(ConversionFailure))?
        .into_rgba8()
        .into_raw();

    Ok(ImageData {
        width: width as usize,
        height: height as usize,
        bytes: bytes.into(),
    })
}

impl Clipboard {
    pub fn get(self: &mut Self) -> Result<Option<ClipboardContext>, UniClipError> {
        let mut hasher = DefaultHasher::new();

        // Try Text
        let state = match self.backend.get_text() {
            Ok(text) => ClipboardState::Text(text),
            Err(e) => match e {
                ContentNotAvailable | ConversionFailure => {
                    match self.backend.get_image() {
                        // Try Image
                        Ok(img) => ClipboardState::Image(encode_as_png(&img)?),
                        Err(e) => {
                            return Err(ClipboardError(e));
                        }
                    }
                }
                _ => {
                    return Err(ClipboardError(e));
                }
            },
        };
        state.hash(&mut hasher);

        let hash = hasher.finish();
        if hash == self.ctx.hash {
            // Do not update the current context when the content is the same.
            return Ok(None);
        }

        let ctx = ClipboardContext {
            stamp: get_utc_now()?,
            state: state,
            hash,
        };

        self.ctx = ctx.clone();
        Ok(Some(ctx))
    }

    pub fn set_text(self: &mut Self, s: String, stamp: u64) -> Result<(), UniClipError> {
        debug!("Clipboard: Trying to set clipboard to: {}", s);
        if stamp <= self.ctx.stamp {
            debug!("Clipboard: Failed - received data is expired.");
            return Ok(()); // Reject setting clipboard as the content is not the latest.
        }

        let state = ClipboardState::Text(s.clone());
        let mut hasher = DefaultHasher::new();
        state.hash(&mut hasher);

        let hash = hasher.finish();
        if hash == self.ctx.hash {
            // Do not update the current context when the content is the same.
            return Ok(());
        }

        let ctx = ClipboardContext {
            stamp: get_utc_now()?,
            state: state,
            hash,
        };

        self.ctx = ctx;

        if let Err(e) = self.backend.set().exclude_from_history().text(s) {
            Err(ClipboardError(e))
        } else {
            Ok(())
        }
    }

    pub fn set_image(self: &mut Self, buf: Vec<u8>, stamp: u64) -> Result<(), UniClipError> {
        debug!("Clipboard: Trying to set clipboard to: <image>");
        if stamp <= self.ctx.stamp {
            debug!("Clipboard: Failed - received data is expired.");
            return Ok(()); // Reject setting clipboard as the content is not the latest.
        }

        let state = ClipboardState::Image(buf.clone());
        let mut hasher = DefaultHasher::new();
        state.hash(&mut hasher);

        let hash = hasher.finish();
        if hash == self.ctx.hash {
            // Do not update the current context when the content is the same.
            return Ok(());
        }

        let ctx = ClipboardContext {
            stamp: get_utc_now()?,
            state: state,
            hash,
        };

        self.ctx = ctx;

        if let Err(e) = self
            .backend
            .set()
            .exclude_from_history()
            .image(read_png(buf.as_slice())?)
        {
            Err(ClipboardError(e))
        } else {
            Ok(())
        }
    }
}

pub struct ClipboardListener {
    clipboard: Clipboard,
}

impl ClipboardListener {
    pub fn new() -> Result<Self, UniClipError> {
        let clipboard = ArBoard::new();
        if let Err(e) = clipboard {
            return Err(ClipboardError(e));
        }

        let backend = clipboard.unwrap();
        let mut clipboard = Clipboard {
            ctx: ClipboardContext {
                hash: 0,
                state: ClipboardState::Text(String::new()),
                stamp: 0,
            },
            backend,
        };

        // Skip local clipboard fetching and wait server's content.
        if !get_option(SYNC_ON_START) {
            let _ = clipboard.get()?;
        }
        Ok(Self { clipboard })
    }

    fn handle_error(e: UniClipError, cancel_token: &CancellationToken) -> bool {
        let s = "LOCAL".to_string();
        match e {
            ClipboardError(ContentNotAvailable)
            | ClipboardError(ClipboardOccupied)
            | ClipboardError(ConversionFailure) => {
                e.report(&s);
                true
            }
            _ => {
                e.panic(&s, &cancel_token);
                false
            }
        }
    }

    pub fn listen(
        mut self: Self,
        mut cmd_rx: mpsc::Receiver<Command>,
        tracker: &TaskTracker,
        cmd_tx: broadcast::Sender<Command>,
        cancel_token: CancellationToken,
    ) -> () {
        let cancel_token_cl = cancel_token.clone();
        let sleep_time = Duration::from_millis(get_option(CLIPBOARD_POLL_FREQ_MS));
        let fetch_send = |manager: &mut Self,
                          send_tx: &broadcast::Sender<Command>,
                          token: &CancellationToken|
         -> bool {
            if manager.clipboard.ctx.stamp == 0 {
                // The clipboard is not initialised.
                // We are still waiting for server's content.
                return true;
            }

            let fetch_res = manager.clipboard.get();
            if let Err(e) = fetch_res {
                if !Self::handle_error(e, token) {
                    return false; // Break the loop
                } else {
                    return true; // Continue
                }
            }
            if let Some(ctx) = fetch_res.unwrap() {
                if let Err(e) = send_tx.send(match ctx.state {
                    ClipboardState::Text(s) => Command::Text(ctx.stamp, s),
                    ClipboardState::Image(buf) => Command::Image(ctx.stamp, buf),
                }) {
                    if !Self::handle_error(ChannelError(e.to_string()), token) {
                        return false; // Break the loop
                    } else {
                        return true; // Continue
                    }
                }
            }
            true
        };
        let listen_loop = async move {
            loop {
                tokio::select! {
                    _ = sleep(sleep_time) => {
                        if !fetch_send(&mut self, &cmd_tx, &cancel_token_cl) {
                            break;
                        }
                    }
                    cmd = cmd_rx.recv() => {
                        match cmd {
                            None => {
                                // We could not handle Channel Error. Exit directly.
                                let _ = Self::handle_error(ChannelError("The channel for receiving commands has been closed.".to_string()), &cancel_token_cl);
                                break;
                            },
                            Some(cmd) => {
                                match cmd {
                                    Command::Sync => {
                                        if let Err(e) = cmd_tx.send(match self.clipboard.ctx.state {
                                            ClipboardState::Text(ref s) => Command::Text(self.clipboard.ctx.stamp, s.clone()),
                                            ClipboardState::Image(ref buf) => Command::Image(self.clipboard.ctx.stamp, buf.clone()),
                                        }) {
                                            if !Self::handle_error(ChannelError(e.to_string()), &cancel_token_cl) {
                                                break;
                                            }
                                            continue;
                                        }

                                    },
                                    Command::Text(stamp, s) => {
                                        if let Err(e) = self.clipboard.set_text(s, stamp) {
                                            if ! Self::handle_error(e, &cancel_token_cl){
                                                break;
                                            }
                                        }
                                    },
                                    Command::Image(stamp, buf) => {
                                        if let Err(e) = self.clipboard.set_image(buf, stamp) {
                                            if ! Self::handle_error(e, &cancel_token_cl){
                                                break;
                                            }
                                        }
                                    },
                                }
                            },
                        }
                    }
                }
            }
        };
        tracker.spawn(cancel_token.run_until_cancelled_owned(listen_loop));
        ()
    }
}
