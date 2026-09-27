//! Versioned interchange only: the postcard representation remains identity.
//!
//! A schema-specific, nonrecursive codec for RFC 8949 §4.2.1 deterministic
//! CBOR. Its only map has fixed unsigned keys 0..3, whose encoded order is
//! their numeric order. No general-purpose CBOR value tree is allocated.

use super::{Manifest, ManifestFile};

pub const MANIFEST_CBOR_VERSION: u64 = 1;
/// Maximum encoded interchange document, inclusive (16 MiB).
pub const MAX_MANIFEST_CBOR_BYTES: usize = 16 * 1024 * 1024;
/// Maximum files in an interchange document, inclusive.
pub const MAX_MANIFEST_CBOR_FILES: usize = 65_536;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestCborError {
    #[error("CBOR manifest exceeds {0} limit")]
    Limit(&'static str),
    #[error("unsupported CBOR manifest version {0}")]
    Version(u64),
    #[error("invalid CBOR manifest at byte {offset}: {reason}")]
    Invalid { offset: usize, reason: &'static str },
}

pub(super) fn encode(manifest: &Manifest) -> Result<Vec<u8>, ManifestCborError> {
    if manifest.files.len() > MAX_MANIFEST_CBOR_FILES {
        return Err(ManifestCborError::Limit("file count"));
    }
    let mut out = Writer(Vec::new());
    out.head(5, 4)?; // map, exactly four keys in deterministic order
    out.head(0, 0)?;
    out.head(0, MANIFEST_CBOR_VERSION)?;
    out.head(0, 1)?;
    out.text(&manifest.name)?;
    out.head(0, 2)?;
    out.head(0, u64::from(manifest.chunk_size))?;
    out.head(0, 3)?;
    out.head(4, manifest.files.len() as u64)?;
    for file in &manifest.files {
        out.head(4, 4)?;
        out.text(&file.path)?;
        out.head(0, file.size)?;
        out.head(2, 32)?;
        out.append(&file.root)?;
        out.text(&file.mime)?;
    }
    Ok(out.0)
}

pub(super) fn decode(bytes: &[u8]) -> Result<Manifest, ManifestCborError> {
    if bytes.len() > MAX_MANIFEST_CBOR_BYTES {
        return Err(ManifestCborError::Limit("byte length"));
    }
    let mut input = Reader { bytes, offset: 0 };
    input.exact(5, 4)?;
    input.exact(0, 0)?;
    let version = input.head(0)?;
    if version != MANIFEST_CBOR_VERSION {
        return Err(ManifestCborError::Version(version));
    }
    input.exact(0, 1)?;
    let name = input.text()?;
    input.exact(0, 2)?;
    let chunk_size =
        u32::try_from(input.head(0)?).map_err(|_| input.invalid("chunk size exceeds u32"))?;
    input.exact(0, 3)?;
    let count = input.head(4)?;
    if count > MAX_MANIFEST_CBOR_FILES as u64 {
        return Err(ManifestCborError::Limit("file count"));
    }
    // Even an empty-path/empty-MIME record needs 38 bytes: array head,
    // text head, size, 32-byte byte-string head+body, text head. Check before
    // reserving a declared count, including on a tiny truncated input.
    if count > (input.remaining() / 38) as u64 {
        return Err(input.invalid("truncated file records"));
    }
    let mut files = Vec::with_capacity(count as usize);
    for _ in 0..count {
        input.exact(4, 4)?;
        let path = input.text()?;
        let size = input.head(0)?;
        input.exact(2, 32)?;
        let mut root = [0; 32];
        root.copy_from_slice(input.take(32)?);
        let mime = input.text()?;
        files.push(ManifestFile {
            path,
            size,
            root,
            mime,
        });
    }
    if input.remaining() != 0 {
        return Err(input.invalid("trailing bytes"));
    }
    // Deliberately do not call Manifest::new: reordering the supplied files
    // would silently change the existing postcard-based content ID.
    Ok(Manifest {
        name,
        chunk_size,
        files,
    })
}

struct Writer(Vec<u8>);

impl Writer {
    fn append(&mut self, bytes: &[u8]) -> Result<(), ManifestCborError> {
        if bytes.len() > MAX_MANIFEST_CBOR_BYTES - self.0.len() {
            return Err(ManifestCborError::Limit("byte length"));
        }
        self.0.extend_from_slice(bytes);
        Ok(())
    }

    fn head(&mut self, major: u8, value: u64) -> Result<(), ManifestCborError> {
        let mut bytes = [0; 9];
        let len = match value {
            0..=23 => {
                bytes[0] = (major << 5) | value as u8;
                1
            }
            24..=255 => {
                bytes[0] = (major << 5) | 24;
                bytes[1] = value as u8;
                2
            }
            256..=65535 => {
                bytes[0] = (major << 5) | 25;
                bytes[1..3].copy_from_slice(&(value as u16).to_be_bytes());
                3
            }
            65536..=4294967295 => {
                bytes[0] = (major << 5) | 26;
                bytes[1..5].copy_from_slice(&(value as u32).to_be_bytes());
                5
            }
            _ => {
                bytes[0] = (major << 5) | 27;
                bytes[1..9].copy_from_slice(&value.to_be_bytes());
                9
            }
        };
        self.append(&bytes[..len])
    }

    fn text(&mut self, text: &str) -> Result<(), ManifestCborError> {
        if text.len() > MAX_MANIFEST_CBOR_BYTES {
            return Err(ManifestCborError::Limit("byte length"));
        }
        self.head(3, text.len() as u64)?;
        self.append(text.as_bytes())
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn invalid(&self, reason: &'static str) -> ManifestCborError {
        ManifestCborError::Invalid {
            offset: self.offset,
            reason,
        }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], ManifestCborError> {
        if count > self.remaining() {
            return Err(self.invalid("truncated item"));
        }
        let start = self.offset;
        self.offset += count;
        Ok(&self.bytes[start..self.offset])
    }

    fn head(&mut self, major: u8) -> Result<u64, ManifestCborError> {
        let initial = self.take(1)?[0];
        if initial >> 5 != major {
            return Err(self.invalid("unexpected type"));
        }
        let (length, minimum) = match initial & 31 {
            value @ 0..=23 => return Ok(u64::from(value)),
            24 => (1, 24),
            25 => (2, 256),
            26 => (4, 65536),
            27 => (8, 4294967296),
            _ => return Err(self.invalid("indefinite or reserved argument")),
        };
        let mut argument = [0; 8];
        argument[8 - length..].copy_from_slice(self.take(length)?);
        let value = u64::from_be_bytes(argument);
        if value < minimum {
            return Err(self.invalid("non-shortest argument"));
        }
        Ok(value)
    }

    fn exact(&mut self, major: u8, expected: u64) -> Result<(), ManifestCborError> {
        if self.head(major)? != expected {
            return Err(self.invalid("unexpected key or container length"));
        }
        Ok(())
    }

    fn text(&mut self) -> Result<String, ManifestCborError> {
        let length = self.head(3)?;
        // Compare as u64 before converting to usize, including on 32-bit hosts.
        if length > self.remaining() as u64 {
            return Err(self.invalid("truncated text"));
        }
        let bytes = self.take(length as usize)?;
        let text = std::str::from_utf8(bytes).map_err(|_| self.invalid("invalid UTF-8"))?;
        Ok(text.to_owned())
    }
}
