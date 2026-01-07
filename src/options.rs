use clap::Parser;
use std::sync::OnceLock;

static _ALLOWED_TIME_DIFF: OnceLock<u64> = OnceLock::new();
static _KEEP_ALIVE: OnceLock<u64> = OnceLock::new();
static _MAX_BUFFER_SIZE: OnceLock<u64> = OnceLock::new();
static _SYNC_INTERVAL: OnceLock<u64> = OnceLock::new();
static _TIMEOUT_SEC: OnceLock<u64> = OnceLock::new();
static _CLIPBOARD_POLL_FREQ_MS: OnceLock<u64> = OnceLock::new();

pub static ALLOWED_TIME_DIFF: &OnceLock<u64> = &_ALLOWED_TIME_DIFF;
pub static KEEP_ALIVE: &OnceLock<u64> = &_KEEP_ALIVE;
pub static MAX_BUFFER_SIZE: &OnceLock<u64> = &_MAX_BUFFER_SIZE;
pub static SYNC_INTERVAL: &OnceLock<u64> = &_SYNC_INTERVAL;
pub static TIMEOUT_SEC: &OnceLock<u64> = &_TIMEOUT_SEC;
pub static CLIPBOARD_POLL_FREQ_MS: &OnceLock<u64> = &_CLIPBOARD_POLL_FREQ_MS;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Args {
    /// Use this option to start the server.
    #[arg(short, default_value_t = false)]
    server_mode: bool,

    /// Server/Bind address. (e.g. "127.0.0.1:12345")
    connection_string: String,

    /// Allowed time difference between client and server.
    #[arg(short, long, default_value_t = 30)]
    allowed_time_diff: u64,

    /// Interval to send keep alive packet.
    #[arg(long, default_value_t = 30)]
    keep_alive: u64,

    /// Maximum message size in MB.
    #[arg(short, long, default_value_t = 50)] // Default 50 MB
    max_message_size: u64,

    /// Interval to sync clipboard between client and server.
    #[arg(long, default_value_t = 60)]
    sync_interval: u64,

    /// Timeout for network requests in second.
    #[arg(long, default_value_t = 3)]
    timeout_sec: u64,

    /// Interval to poll from the system clipboard in millisecond.
    #[arg(long, default_value_t = 500)]
    clipboard_poll_freq_ms: u64,
}

#[inline]
pub fn get_option<T: Clone>(option: &OnceLock<T>) -> T {
    // Safety: Options are only used after it was initialised.
    option.get().unwrap().clone()
}

#[inline]
pub fn set_options() -> (bool, String) {
    // Do not call this function twice.
    // Or it will panic.
    let args = Args::parse();
    _ALLOWED_TIME_DIFF.set(args.allowed_time_diff).unwrap();
    _KEEP_ALIVE.set(args.keep_alive).unwrap();
    _MAX_BUFFER_SIZE
        .set(args.max_message_size * 1024 * 1024)
        .unwrap();
    _SYNC_INTERVAL.set(args.sync_interval).unwrap();
    _TIMEOUT_SEC.set(args.timeout_sec).unwrap();
    _CLIPBOARD_POLL_FREQ_MS
        .set(args.clipboard_poll_freq_ms)
        .unwrap();

    (args.server_mode, args.connection_string)
}
