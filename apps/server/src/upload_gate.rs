//! One gate for every surface that files bytes into the library: the app's
//! inline and ticketed uploads, Hotline, ZMODEM, and pulls from other
//! burrows. Each surface calls [`check`] when the size is first declared, so
//! a file that cannot be kept is refused before any of it is sent, and again
//! on the bytes that actually arrived, because a declared size is only a
//! claim.
//!
//! Two limits, both the operator's and both live:
//!
//! - `upload_max_file_bytes`, the largest single file (0 = no limit). A
//!   surface's own hard ceiling (the inline frame, Hotline's and ZMODEM's
//!   in-memory buffers) still applies beneath it: see [`file_ceiling`].
//! - `upload_quota_bytes`, what one account may keep altogether (0 = none).

use rabbithole_proto::ErrorCode;

use crate::Shared;

/// Why an upload is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Bigger than the largest file this burrow takes.
    TooBig { max: u64 },
    /// It would put the account over its space.
    OverQuota { quota: u64, used: u64 },
    /// The space in use could not be read, so it cannot be vouched for.
    Unavailable,
}

impl Refusal {
    /// The code a client is sent: a limit reads as "too large".
    pub fn code(self) -> ErrorCode {
        match self {
            Refusal::TooBig { .. } | Refusal::OverQuota { .. } => ErrorCode::TooLarge,
            Refusal::Unavailable => ErrorCode::Internal,
        }
    }

    /// A line for a text surface (Hotline, telnet), in the operator's terms.
    pub fn line(self) -> String {
        match self {
            Refusal::TooBig { max } => {
                format!(
                    "file too large: this burrow takes files up to {}",
                    size(max)
                )
            }
            Refusal::OverQuota { quota, used } => format!(
                "storage quota exceeded: {} of {} is already in use",
                size(used),
                size(quota)
            ),
            Refusal::Unavailable => "the file library is unavailable; try again later".into(),
        }
    }
}

/// A size as the console writes it: `50 MiB`, `1.5 GiB`, `900 KiB`, `12 bytes`.
pub fn size(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("TiB", 1 << 40),
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
    ];
    for (unit, scale) in UNITS {
        if bytes >= scale {
            let whole = bytes / scale;
            let tenths = (bytes % scale) * 10 / scale;
            return if tenths == 0 {
                format!("{whole} {unit}")
            } else {
                format!("{whole}.{tenths} {unit}")
            };
        }
    }
    format!("{bytes} bytes")
}

/// The largest file this burrow takes, or `None` for no limit.
pub fn max_file_bytes(shared: &Shared) -> Option<u64> {
    match shared.config.read().upload_max_file_bytes {
        0 => None,
        max => Some(max),
    }
}

/// The ceiling a surface enforces while bytes stream in: the operator's
/// per-file limit, or the surface's own hard cap if that is lower.
pub fn file_ceiling(shared: &Shared, surface_cap: u64) -> u64 {
    max_file_bytes(shared).map_or(surface_cap, |max| max.min(surface_cap))
}

/// May `account_id` keep one more file of `size` bytes? Checks the per-file
/// limit first (it needs no database), then the account's space.
///
/// To file what passed, hold [`commit_lock`] from before this check until the
/// file is recorded: otherwise two uploads by one account can each fit the
/// last of its space, and both land.
pub async fn check(shared: &Shared, account_id: i64, size: u64) -> Result<(), Refusal> {
    if let Some(max) = max_file_bytes(shared) {
        if size > max {
            return Err(Refusal::TooBig { max });
        }
    }
    check_quota(shared, account_id, size).await
}

/// Would `more` bytes still fit in `account_id`'s space? A store that cannot
/// say refuses rather than waving the upload through.
pub async fn check_quota(shared: &Shared, account_id: i64, more: u64) -> Result<(), Refusal> {
    let quota = shared.config.read().upload_quota_bytes;
    if quota == 0 {
        return Ok(());
    }
    let used = shared
        .files
        .uploaded_bytes(account_id)
        .await
        .map_err(|_| Refusal::Unavailable)?
        .max(0) as u64;
    if used.saturating_add(more) > quota {
        return Err(Refusal::OverQuota { quota, used });
    }
    Ok(())
}

/// Held while a check and the filing it allows happen together. One lock for
/// the whole burrow: filing is quick, and quota is per account.
pub async fn commit_lock(shared: &Shared) -> tokio::sync::MutexGuard<'_, ()> {
    shared.transfers.commit_lock.lock().await
}

/// What `account_id` already keeps in the library, in bytes (0 when the
/// store cannot say: this only informs, the checks above decide).
pub async fn used_bytes(shared: &Shared, account_id: i64) -> u64 {
    shared
        .files
        .uploaded_bytes(account_id)
        .await
        .unwrap_or(0)
        .max(0) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_says_which_limit_it_hit_in_the_consoles_units() {
        assert_eq!(Refusal::TooBig { max: 10 }.code(), ErrorCode::TooLarge);
        assert_eq!(Refusal::Unavailable.code(), ErrorCode::Internal);
        assert_eq!(
            Refusal::TooBig { max: 50 << 20 }.line(),
            "file too large: this burrow takes files up to 50 MiB"
        );
        assert_eq!(
            Refusal::OverQuota {
                quota: 1 << 30,
                used: 900 << 20
            }
            .line(),
            "storage quota exceeded: 900 MiB of 1 GiB is already in use"
        );
        assert_eq!(size(12), "12 bytes");
        assert_eq!(size(1536), "1.5 KiB");
        assert_eq!(size((5 << 30) + (1 << 29)), "5.5 GiB");
    }
}
