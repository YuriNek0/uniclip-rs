use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::UniClipError::{self, *};

#[inline]
pub fn get_utc_now() -> Result<u64, UniClipError> {
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return Err(SystemClockError);
    };
    Ok(now.as_secs())
}
