//! 64-byte wPRF v1 file header.
//!
//! Binary layout (ADR-010, final-design.md §4.1):
//! ```text
//!   magic:                    [u8; 4]   "wPRF"
//!   version:                  u8        1
//!   endianness:               u8        1 = LE
//!   host_arch:                u8        0 = x86_64, 1 = aarch64
//!   _reserved:                u8        0
//!   data_section_end_offset:  u64       crash recovery bookmark
//!   section_table_offset:     u64       footer location (0 if absent)
//!   feature_bitmap:           [u8; 32]  256-bit capability flags
//!   reserved_padding:         [u8; 8]   align to 64B
//! ```

use std::io::{self, Read, Write};

/// File magic bytes: ASCII "wPRF".
pub const MAGIC: [u8; 4] = *b"wPRF";

/// Current format version.
pub const VERSION: u8 = 1;

/// Endianness marker: 1 = little-endian.
pub const ENDIAN_LE: u8 = 1;

/// Host architecture codes.
pub const ARCH_X86_64: u8 = 0;
pub const ARCH_AARCH64: u8 = 1;

/// Total header size in bytes.
pub const HEADER_SIZE: usize = 64;

/// 64-byte file header for .wperf files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WprfHeader {
    pub version: u8,
    pub endianness: u8,
    pub host_arch: u8,
    pub data_section_end_offset: u64,
    pub section_table_offset: u64,
    pub feature_bitmap: [u8; 32],
}

impl WprfHeader {
    /// Create a new header with defaults for the current host.
    pub fn new() -> Self {
        Self {
            version: VERSION,
            endianness: ENDIAN_LE,
            host_arch: detect_arch(),
            data_section_end_offset: HEADER_SIZE as u64,
            section_table_offset: 0,
            feature_bitmap: [0u8; 32],
        }
    }

    /// Serialize the header to exactly 64 bytes (little-endian).
    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(&MAGIC);
        buf[4] = self.version;
        buf[5] = self.endianness;
        buf[6] = self.host_arch;
        buf[7] = 0; // _reserved
        buf[8..16].copy_from_slice(&self.data_section_end_offset.to_le_bytes());
        buf[16..24].copy_from_slice(&self.section_table_offset.to_le_bytes());
        buf[24..56].copy_from_slice(&self.feature_bitmap);
        // buf[56..64] = reserved_padding, already zeroed
        buf
    }

    /// Write the header to a writer.
    pub fn write_to<W: Write>(&self, w: &mut W) -> io::Result<()> {
        w.write_all(&self.to_bytes())
    }

    /// Parse a header from exactly 64 bytes.
    pub fn from_bytes(buf: &[u8; HEADER_SIZE]) -> Result<Self, HeaderError> {
        if buf[0..4] != MAGIC {
            return Err(HeaderError::BadMagic);
        }
        let version = buf[4];
        if version != VERSION {
            return Err(HeaderError::UnsupportedVersion(version));
        }
        let endianness = buf[5];
        let host_arch = buf[6];
        let data_section_end_offset = u64::from_le_bytes(buf[8..16].try_into().unwrap());
        let section_table_offset = u64::from_le_bytes(buf[16..24].try_into().unwrap());
        let mut feature_bitmap = [0u8; 32];
        feature_bitmap.copy_from_slice(&buf[24..56]);

        Ok(Self {
            version,
            endianness,
            host_arch,
            data_section_end_offset,
            section_table_offset,
            feature_bitmap,
        })
    }

    /// Read and parse a header from a reader.
    pub fn read_from<R: Read>(r: &mut R) -> Result<Self, HeaderError> {
        let mut buf = [0u8; HEADER_SIZE];
        r.read_exact(&mut buf).map_err(HeaderError::Io)?;
        Self::from_bytes(&buf)
    }
}

impl Default for WprfHeader {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors when parsing a wPRF header.
#[derive(Debug)]
pub enum HeaderError {
    BadMagic,
    UnsupportedVersion(u8),
    Io(io::Error),
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMagic => write!(f, "invalid wPRF magic bytes"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported wPRF version: {v}"),
            Self::Io(e) => write!(f, "I/O error reading header: {e}"),
        }
    }
}

impl std::error::Error for HeaderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

fn detect_arch() -> u8 {
    if cfg!(target_arch = "x86_64") {
        ARCH_X86_64
    } else if cfg!(target_arch = "aarch64") {
        ARCH_AARCH64
    } else {
        255 // unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_roundtrip() {
        let mut h = WprfHeader::new();
        h.data_section_end_offset = 12345;
        h.section_table_offset = 67890;
        h.feature_bitmap[0] = 0xFF;
        h.feature_bitmap[31] = 0x42;

        let bytes = h.to_bytes();
        assert_eq!(bytes.len(), HEADER_SIZE);

        let parsed = WprfHeader::from_bytes(&bytes).unwrap();
        assert_eq!(h, parsed);
    }

    #[test]
    fn header_magic_check() {
        let mut buf = [0u8; HEADER_SIZE];
        buf[0..4].copy_from_slice(b"NOPE");
        assert!(matches!(
            WprfHeader::from_bytes(&buf),
            Err(HeaderError::BadMagic)
        ));
    }

    #[test]
    fn header_version_check() {
        let mut h = WprfHeader::new();
        h.version = 99;
        let bytes = h.to_bytes();
        // Manually fix the magic but keep version=99
        assert!(matches!(
            WprfHeader::from_bytes(&bytes),
            Err(HeaderError::UnsupportedVersion(99))
        ));
    }

    #[test]
    fn header_size_is_64() {
        assert_eq!(HEADER_SIZE, 64);
        assert_eq!(WprfHeader::new().to_bytes().len(), 64);
    }

    #[test]
    fn header_error_display() {
        // Kill mutation: Display::fmt → Ok(Default::default())
        let err = HeaderError::BadMagic;
        let msg = format!("{err}");
        assert!(!msg.is_empty());
        assert!(msg.contains("magic"));

        let err = HeaderError::UnsupportedVersion(99);
        assert!(format!("{err}").contains("99"));

        let err = HeaderError::Io(std::io::Error::other("test"));
        assert!(format!("{err}").contains("test"));
    }

    #[test]
    fn header_error_source() {
        // Kill mutations: Error::source → None, delete Self::Io(e) arm
        let io_err = HeaderError::Io(std::io::Error::other("x"));
        assert!(std::error::Error::source(&io_err).is_some());

        let bad_magic = HeaderError::BadMagic;
        assert!(std::error::Error::source(&bad_magic).is_none());
    }

    #[test]
    fn header_io_roundtrip() {
        let h = WprfHeader::new();
        let mut buf = Vec::new();
        h.write_to(&mut buf).unwrap();
        assert_eq!(buf.len(), 64);

        let parsed = WprfHeader::read_from(&mut buf.as_slice()).unwrap();
        assert_eq!(h, parsed);
    }
}
