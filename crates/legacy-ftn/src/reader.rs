//! A tiny bounds-checked cursor over a byte slice.
//!
//! FidoNet packets are little-endian and full of fixed-width and
//! NUL-terminated fields. Every accessor here is fallible: an over-read
//! yields [`FtnError::Truncated`] rather than panicking, which is what makes
//! the whole codec total on arbitrary input.

use crate::error::FtnError;

/// Bounds-checked little-endian reader over a byte slice.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Wrap a slice.
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    /// Bytes not yet consumed.
    pub(crate) fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn need(&self, n: usize) -> Result<(), FtnError> {
        if self.remaining() < n {
            Err(FtnError::Truncated {
                at: self.pos,
                need: n,
                len: self.buf.len(),
            })
        } else {
            Ok(())
        }
    }

    /// Read a single byte.
    pub(crate) fn u8(&mut self) -> Result<u8, FtnError> {
        self.need(1)?;
        let b = self.buf[self.pos];
        self.pos += 1;
        Ok(b)
    }

    /// Read a little-endian `u16`.
    pub(crate) fn u16_le(&mut self) -> Result<u16, FtnError> {
        self.need(2)?;
        let v = u16::from_le_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }

    /// Read a little-endian `u32`.
    pub(crate) fn u32_le(&mut self) -> Result<u32, FtnError> {
        self.need(4)?;
        let v = u32::from_le_bytes([
            self.buf[self.pos],
            self.buf[self.pos + 1],
            self.buf[self.pos + 2],
            self.buf[self.pos + 3],
        ]);
        self.pos += 4;
        Ok(v)
    }

    /// Peek the next little-endian `u16` without consuming it. Returns `None`
    /// when fewer than two bytes remain.
    pub(crate) fn peek_u16_le(&self) -> Option<u16> {
        if self.remaining() < 2 {
            None
        } else {
            Some(u16::from_le_bytes([
                self.buf[self.pos],
                self.buf[self.pos + 1],
            ]))
        }
    }

    /// Read exactly `N` bytes into a fixed-size array.
    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], FtnError> {
        self.need(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(&self.buf[self.pos..self.pos + N]);
        self.pos += N;
        Ok(out)
    }

    /// Read a NUL-terminated field, consuming the terminating NUL. If no NUL
    /// is found before end-of-buffer, all remaining bytes are returned (the
    /// field is treated as running to the end). The returned slice excludes
    /// the NUL.
    pub(crate) fn cstr(&mut self) -> &'a [u8] {
        let start = self.pos;
        while self.pos < self.buf.len() && self.buf[self.pos] != 0 {
            self.pos += 1;
        }
        let end = self.pos;
        if self.pos < self.buf.len() {
            // consume the NUL
            self.pos += 1;
        }
        &self.buf[start..end]
    }
}
