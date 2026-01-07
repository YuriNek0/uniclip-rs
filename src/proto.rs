use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio::time::{Duration, Instant, sleep_until, timeout};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use zerocopy::{IntoBytes, try_transmute};
use zerocopy_derive::*;

use std::io::ErrorKind;

use rand::Rng;
use rand::SeedableRng;
use rand::rngs::SmallRng;

use crate::error::UniClipError::{self, *};
use crate::options::*;
use crate::timestamp::get_utc_now;

const MAGIC_BYTES: [u8; 8] = *b"UNICLIP!";
const TIME_RAND_RANGE: u32 = 10;

#[derive(Unaligned, Immutable, TryFromBytes, IntoBytes, PartialEq, Eq)]
#[repr(u8)]
enum MessageKind {
    HandShake,
    Sync,
    Error,
    Text,
    Image,
    Disconnect,
}

#[derive(Unaligned, Immutable, TryFromBytes, IntoBytes, KnownLayout)]
#[repr(packed)]
struct MessageMeta {
    magic: [u8; 8],
    len: u32,
    stamp: u64,
    kind: MessageKind,
}

pub struct Message {
    meta: MessageMeta,
    content: Vec<u8>,
}

#[derive(Clone)]
pub enum Command {
    Sync,
    Text(u64, String),
    Image(u64, Vec<u8>),
}

pub struct Connection {
    stream: TcpStream,
    initial_handshake_done: bool,
    cmd_req_tx: mpsc::Sender<Command>,
    cmd_recv_rx: broadcast::Receiver<Command>,

    last_keep_alive: u64,
    last_sync: u64,
}

impl Message {
    async fn read_from_stream(stream: &mut TcpStream) -> Result<Self, UniClipError> {
        const META_SIZE: usize = std::mem::size_of::<MessageMeta>();
        let mut meta_buf: [u8; META_SIZE] = [0; META_SIZE];
        match timeout(
            Duration::from_secs(get_option(TIMEOUT_SEC)),
            stream.read_exact(meta_buf.as_mut_slice()),
        )
        .await
        {
            Ok(Ok(0)) => return Err(DisconnectedError),
            Ok(Ok(n)) => {
                if n != META_SIZE {
                    return Err(PacketParseError("Incomplete message received.".to_string()));
                }
            }
            Ok(Err(e)) => return Err(IOError(e.to_string())),
            Err(_) => return Err(TimeoutError),
        };

        let meta = match try_transmute!(meta_buf) {
            Ok(v) => v,
            Err(e) => {
                let _: zerocopy::ValidityError<[u8; META_SIZE], MessageMeta> = e;
                return Err(PacketParseError(e.to_string()));
            }
        };

        if meta.magic != MAGIC_BYTES {
            return Err(PacketParseError("Magic value mismatch!".to_string()));
        }

        if meta.len > 0 {
            if meta.kind != MessageKind::Text && meta.kind != MessageKind::Image {
                return Err(PacketParseError(
                    "Non-empty data received in control messages.".to_string(),
                ));
            }
            let mut msg_buf: Vec<u8> = vec![0; meta.len as usize];
            match timeout(
                Duration::from_secs(get_option(TIMEOUT_SEC)),
                stream.read_exact(msg_buf.as_mut_slice()),
            )
            .await
            {
                Err(_) => return Err(TimeoutError),
                Ok(Ok(0)) => return Err(DisconnectedError),
                Ok(Ok(n)) => {
                    if n != (meta.len as usize) {
                        return Err(PacketParseError("Incomplete message received.".to_string()));
                    }
                }
                Ok(Err(e)) => return Err(IOError(e.to_string())),
            };

            Ok(Self {
                meta,
                content: msg_buf,
            })
        } else {
            Ok(Self {
                meta,
                content: Vec::new(),
            })
        }
    }

    fn new(kind: MessageKind, content: Vec<u8>, stamp: Option<u64>) -> Result<Self, UniClipError> {
        let msg_stamp = stamp.unwrap_or(get_utc_now()?);
        if content.len() > get_option(MAX_BUFFER_SIZE) as usize {
            return Err(MessageTooLargeError);
        }

        Ok(Self {
            meta: MessageMeta {
                magic: MAGIC_BYTES,
                kind: kind,
                len: content.len() as u32,
                stamp: msg_stamp,
            },
            content: content,
        })
    }

    fn to_packet(self: &Self) -> Vec<u8> {
        [self.meta.as_bytes(), &self.content].concat()
    }
}

impl Connection {
    pub async fn spawn_connect(
        addr: String,
        tracker: &TaskTracker,
        client_token: CancellationToken,
        cmd_req_tx: mpsc::Sender<Command>,
        cmd_recv_rx: broadcast::Receiver<Command>,
    ) -> () {
        let stream = match timeout(
            Duration::from_secs(get_option(TIMEOUT_SEC)),
            TcpStream::connect(&addr),
        )
        .await
        {
            Ok(Ok(stream)) => stream,
            Ok(Err(e)) => match e.kind() {
                ErrorKind::ConnectionRefused
                | ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::HostUnreachable
                | ErrorKind::NetworkUnreachable
                | ErrorKind::NotConnected => {
                    ConnectionError(format!("Could not connect to {}.", &addr))
                        .panic(&"LOCAL".to_string(), &client_token);
                    return;
                }
                _ => {
                    IOError(e.to_string()).panic(&"LOCAL".to_string(), &client_token);
                    return;
                }
            },
            Err(_) => {
                TimeoutError.panic(&"LOCAL".to_string(), &client_token);
                return;
            }
        };

        let mut conn = Self {
            stream,
            initial_handshake_done: false,
            cmd_req_tx,
            cmd_recv_rx,
            last_keep_alive: 0,
            last_sync: 0,
        };
        let _ = conn.expect_handshake().await;

        tracker.spawn(conn.connection_handler(addr.to_string(), client_token, false));
    }

    pub fn spawn_serve(
        addr: String,
        tracker: &TaskTracker,
        cancel_token: CancellationToken,
        cmd_req_tx_arg: mpsc::Sender<Command>,
        cmd_recv_rx_arg: broadcast::Receiver<Command>,
    ) -> () {
        let closure = async move |_tracker: TaskTracker| {
            let listener = match TcpListener::bind(addr).await {
                Ok(v) => v,
                Err(e) => {
                    IOError(e.to_string()).panic(&"LOCAL".to_string(), &cancel_token);
                    return;
                }
            };

            cancel_token
                .run_until_cancelled(async {
                    loop {
                        let cmd_req_tx = cmd_req_tx_arg.clone();
                        let cmd_recv_rx = cmd_recv_rx_arg.resubscribe();
                        let (stream, client_addr) = match listener.accept().await {
                            Ok((stream, client_addr)) => (stream, client_addr),
                            Err(e) => match e.kind() {
                                ErrorKind::WouldBlock
                                | ErrorKind::ConnectionAborted
                                | ErrorKind::ConnectionReset => {
                                    let _ =
                                        ConnectionError(e.to_string()).report(&"LOCAL".to_string());
                                    continue;
                                }
                                _ => {
                                    IOError(e.to_string())
                                        .panic(&"LOCAL".to_string(), &cancel_token);
                                    return;
                                }
                            },
                        };

                        let conn = Self {
                            stream,
                            initial_handshake_done: false,
                            cmd_req_tx,
                            cmd_recv_rx,
                            last_keep_alive: 0,
                            last_sync: 0,
                        };

                        // Each client has their own cancellation token.
                        // When the server token gets canceled, tokens for clients will be cancelled as well.
                        // Server needs to send handshake first.
                        let client_token = cancel_token.child_token();
                        _tracker.spawn(conn.connection_handler(
                            client_addr.to_string(),
                            client_token.clone(),
                            true,
                        ));
                    }
                })
                .await;
        };
        tracker.spawn(closure(tracker.clone()));
        ()
    }

    async fn connection_handler(
        mut self: Self,
        addr: String,
        cancel_token: CancellationToken,
        init_send_handshake: bool,
    ) -> () {
        let closure = async |conn: &mut Self| -> Result<(), UniClipError> {
            let mut rng = SmallRng::from_os_rng();
            loop {
                let now = get_utc_now()?;
                let next_keep_alive = conn.last_keep_alive
                    + get_option(KEEP_ALIVE)
                    + rng.random_range(0..TIME_RAND_RANGE) as u64;
                let next_sync = conn.last_sync
                    + get_option(SYNC_INTERVAL)
                    + rng.random_range(0..TIME_RAND_RANGE) as u64;

                if now > next_sync {
                    conn.req_sync().await?
                }

                if now > next_keep_alive {
                    conn.keep_alive().await?
                }

                // Cancellation safety: All selected arms are cancel safe.
                tokio::select! {
                    biased;
                    _ = cancel_token.cancelled() => {
                        // Task has been canceled. Try to send a disconnect message.
                        if let Ok(msg) = Message::new(
                            MessageKind::Disconnect,
                            Vec::new(),
                            None,
                        ) {
                            let _ = conn.stream.try_write(msg.to_packet().as_bytes());
                        }
                        break;
                    }
                    _ = sleep_until(Instant::now() + Duration::from_secs(next_sync - now)) => {
                        conn.req_sync().await?
                    }

                    _ = sleep_until(Instant::now() + Duration::from_secs(next_keep_alive - now)) => {
                        conn.req_sync().await?
                    }

                    recv_result = conn.cmd_recv_rx.recv() => {
                        match recv_result {
                            Err(e) => { return Err(ChannelError(e.to_string())); },
                            Ok(cmd) => {
                                match cmd {
                                    Command::Sync => { return Err(UnknownError("Received Sync command from the channel.".to_string())); },
                                    Command::Text(stamp, s) => conn.send_text(&s, stamp).await?,
                                    Command::Image(stamp, b) => conn.send_image(&b, stamp).await?,
                                }
                            }
                        }
                    }

                    _ = conn.stream.readable() => {
                        let msg = Message::read_from_stream(&mut conn.stream).await?;
                        conn.handle_msg(&msg).await?
                    }
                };
            }
            Ok(())
        };
        if init_send_handshake {
            // Send handshake to the client and expecting a returned message
            if let Err(e) = self.keep_alive().await {
                let _ = e.panic(&addr, &cancel_token);
                return ();
            }
        }
        loop {
            let ret = closure(&mut self).await;
            if ret.is_ok() {
                break;
            }

            let result = ret.unwrap_err();
            match result {
                UniClipError::DisconnectedError => {
                    info!("[{}] Client Disconnected!", addr);
                    cancel_token.cancel();
                    break;
                }
                UniClipError::ClipboardError(_) => {
                    let s = "LOCAL".to_string();
                    result.report(&s);
                    break;
                }
                _ => {
                    if let Ok(err_msg) = Message::new(
                        MessageKind::Error,
                        result.as_ref().to_string().into_bytes(),
                        None,
                    ) {
                        let _ = self.stream.try_write(err_msg.to_packet().as_bytes());
                    }
                    result.panic(&addr, &cancel_token)
                }
            }
        }
        ()
    }

    async fn expect_handshake(self: &mut Self) -> Result<(), UniClipError> {
        let closure = async move {
            loop {
                let msg = Message::read_from_stream(&mut self.stream).await?;
                if msg.meta.kind == MessageKind::HandShake {
                    // Check if system clocks are synced.
                    if get_utc_now()?.abs_diff(msg.meta.stamp)
                        > ALLOWED_TIME_DIFF.get().unwrap().clone()
                    {
                        return Err(SystemClockError);
                    }

                    self.initial_handshake_done = true;
                    return Ok(());
                }
                if !self.initial_handshake_done {
                    return Err(PacketParseError(
                        "Message received before handshake.".to_string(),
                    ));
                }
                self.handle_msg(&msg).await?;
            }
        };

        match timeout(Duration::from_secs(get_option(TIMEOUT_SEC) * 3), closure).await {
            Ok(v) => v,
            Err(_) => return Err(TimeoutError),
        }
    }

    async fn send_handshake(self: &mut Self) -> Result<(), UniClipError> {
        let req_msg = Message::new(MessageKind::HandShake, Vec::new(), None)?.to_packet();
        match timeout(
            Duration::from_secs(get_option(TIMEOUT_SEC)),
            self.stream.write_all(&req_msg),
        )
        .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(ConnectionError(e.to_string())),
            Err(_) => Err(TimeoutError),
        }
    }

    async fn handle_msg(self: &mut Self, msg: &Message) -> Result<(), UniClipError> {
        match msg.meta.kind {
            MessageKind::HandShake => {
                self.last_keep_alive = get_utc_now()?;
                self.send_handshake().await
            }
            MessageKind::Error => Err(ClientError(
                std::str::from_utf8(&msg.content)
                    .unwrap_or("Unknown Error - Unable to decode error message.")
                    .to_string(),
            )),
            MessageKind::Text => {
                self.last_keep_alive = get_utc_now()?;
                self.last_sync = get_utc_now()?;
                if msg.meta.stamp == 0 {
                    return Ok(()); // Ignore messages with stamp 0 - Error 
                }
                let Ok(s) = std::str::from_utf8(&msg.content) else {
                    return Err(PacketParseError(String::from(
                        "Unable to decode UTF-8 text from the client.",
                    )));
                };
                let s = s.to_string();
                if let Err(e) = self.cmd_req_tx.send(Command::Text(msg.meta.stamp, s)).await {
                    Err(ChannelError(e.to_string()))
                } else {
                    Ok(())
                }
            }
            MessageKind::Sync => {
                self.last_keep_alive = get_utc_now()?;
                self.last_sync = get_utc_now()?;
                if let Err(e) = self.cmd_req_tx.send(Command::Sync).await {
                    Err(ChannelError(e.to_string()))
                } else {
                    Ok(())
                }
            }
            MessageKind::Image => {
                self.last_keep_alive = get_utc_now()?;
                self.last_sync = get_utc_now()?;
                if let Err(e) = self
                    .cmd_req_tx
                    .send(Command::Image(msg.meta.stamp, msg.content.clone()))
                    .await
                {
                    Err(ChannelError(e.to_string()))
                } else {
                    Ok(())
                }
            }
            MessageKind::Disconnect => Err(DisconnectedError),
        }
    }

    async fn keep_alive(self: &mut Self) -> Result<(), UniClipError> {
        self.send_handshake().await?;
        self.last_keep_alive = get_utc_now()?;
        self.expect_handshake().await
    }

    async fn req_sync(self: &mut Self) -> Result<(), UniClipError> {
        let req_msg = Message::new(MessageKind::Sync, Vec::new(), None)?.to_packet();
        self.last_keep_alive = get_utc_now()?;
        self.last_sync = get_utc_now()?;
        let sync_closure = async move {
            loop {
                match timeout(
                    Duration::from_secs(get_option(TIMEOUT_SEC)),
                    self.stream.write_all(&req_msg),
                )
                .await
                {
                    Ok(Ok(v)) => v,
                    Ok(Err(e)) => return Err(ConnectionError(e.to_string())),
                    Err(_) => return Err(TimeoutError),
                };

                let msg = Message::read_from_stream(&mut self.stream).await?;
                self.handle_msg(&msg).await?;
                if msg.meta.kind == MessageKind::Image || msg.meta.kind == MessageKind::Text {
                    return Ok(());
                }
            }
        };

        match timeout(
            Duration::from_secs(get_option(TIMEOUT_SEC) * 3),
            sync_closure,
        )
        .await
        {
            Ok(v) => v,
            Err(_) => Err(TimeoutError),
        }
    }

    async fn send_text(
        self: &mut Self,
        clipboard: &String,
        stamp: u64,
    ) -> Result<(), UniClipError> {
        let msg = Message::new(
            MessageKind::Text,
            clipboard.clone().into_bytes(),
            Some(stamp),
        )?
        .to_packet();
        self.last_keep_alive = get_utc_now()?;
        self.last_sync = get_utc_now()?;
        match timeout(
            Duration::from_secs(get_option(TIMEOUT_SEC)),
            self.stream.write_all(&msg),
        )
        .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(ConnectionError(e.to_string())),
            Err(_) => Err(TimeoutError),
        }
    }

    async fn send_image(self: &mut Self, clipboard: &[u8], stamp: u64) -> Result<(), UniClipError> {
        let msg = Message::new(MessageKind::Text, clipboard.to_vec(), Some(stamp))?.to_packet();
        self.last_keep_alive = get_utc_now()?;
        self.last_sync = get_utc_now()?;
        match timeout(
            Duration::from_secs(get_option(TIMEOUT_SEC)),
            self.stream.write_all(&msg),
        )
        .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(ConnectionError(e.to_string())),
            Err(_) => Err(TimeoutError),
        }
    }
}
