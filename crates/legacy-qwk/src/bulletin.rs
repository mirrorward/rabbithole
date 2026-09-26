//! Optional global bulletins are separate `BLT-0.<number>` packet members.
//! Names are generated from their list position, never accepted as paths.
//!
//! The QWK bulletin convention uses unpadded numbers and ASCII/ANSI text:
//! <https://wmcbrine.com/mmail/specs/qwkmay.html>. The codec accepts already
//! encoded bytes; the server adapter supplies CP437 text with CRLF lines.

use crate::QwkError;

/// Maximum optional bulletins in one packet.
pub const MAX_BULLETINS: usize = 32;
/// Maximum bytes in one bulletin.
pub const MAX_BULLETIN_BYTES: usize = 64 * 1024;
/// Maximum combined bulletin bytes in one packet.
pub const MAX_BULLETINS_BYTES: usize = 256 * 1024;

/// Check the per-item, count and aggregate bounds without copying content.
/// Adapters can check source text before encoding; packet assembly checks
/// the final encoded bytes too.
pub fn validate_sizes(sizes: impl IntoIterator<Item = usize>) -> Result<(), QwkError> {
    let mut total = 0;
    for (index, size) in sizes.into_iter().enumerate() {
        for (limit, maximum, actual) in [
            ("count", MAX_BULLETINS, index + 1),
            ("bytes per bulletin", MAX_BULLETIN_BYTES, size),
        ] {
            if actual > maximum {
                return Err(QwkError::BulletinLimit {
                    limit,
                    maximum,
                    actual,
                });
            }
        }
        // Count and individual sizes are already bounded, so this cannot
        // overflow even on a 32-bit target.
        total += size;
        if total > MAX_BULLETINS_BYTES {
            return Err(QwkError::BulletinLimit {
                limit: "total bytes",
                maximum: MAX_BULLETINS_BYTES,
                actual: total,
            });
        }
    }
    Ok(())
}
