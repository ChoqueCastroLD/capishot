//! Merged (composite) image data section.

use std::borrow::Cow;

use crate::compression::{Compression, PlaneLayout, decode_planes, encode_planes, validate_planes};
use crate::error::Result;
use crate::header::Header;
use crate::io::Reader;
use crate::source::Stored;

/// The merged image: compression plus the encoded bytes of all channels
/// (to the end of the file), kept verbatim.
///
/// As with [`crate::ChannelData`], a file parsed by
/// [`PsdFile::from_source`](crate::PsdFile::from_source) may leave the bytes in the source
/// (`stored`, with `data` empty); equality compares the bytes wherever they are.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ImageData {
    /// Compression method for the whole image.
    pub compression: Compression,
    /// Encoded data. For RLE: the byte-count table for every row of every
    /// channel followed by the rows.
    pub data: Vec<u8>,
    /// Where the encoded bytes are when they were left in the source. (Not serialized.)
    #[cfg_attr(feature = "serde", serde(skip))]
    pub stored: Option<Stored>,
}

impl PartialEq for ImageData {
    fn eq(&self, other: &Self) -> bool {
        if self.compression != other.compression || self.encoded_len() != other.encoded_len() {
            return false;
        }
        match (self.encoded(), other.encoded()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for ImageData {}

impl ImageData {
    /// A merged image whose encoded bytes are in memory.
    pub fn new(compression: Compression, data: Vec<u8>) -> Self {
        ImageData { compression, data, stored: None }
    }

    /// The encoded bytes: borrowed from `data`, or read from the source.
    pub fn encoded(&self) -> Result<Cow<'_, [u8]>> {
        crate::source::encoded(&self.data, self.stored.as_ref())
    }

    /// Number of encoded bytes, wherever they are.
    pub fn encoded_len(&self) -> u64 {
        self.stored.as_ref().map_or(self.data.len() as u64, |s| s.len)
    }

    /// Reads bytes left in the source into `data`.
    pub fn load(&mut self) -> Result<()> {
        if let Some(s) = &self.stored {
            self.data = s.read()?;
            self.stored = None;
        }
        Ok(())
    }

    /// Plane layout for the merged image described by `header`.
    pub fn layout(header: &Header) -> PlaneLayout {
        PlaneLayout {
            planes: usize::from(header.channels),
            width: header.width as usize,
            height: header.height as usize,
            depth: header.depth,
            version: header.version,
        }
    }

    /// Encodes planar data (`channels × height × row_bytes`).
    pub fn encode(compression: Compression, decoded: &[u8], header: &Header) -> Result<Self> {
        Ok(ImageData::new(compression, encode_planes(compression, decoded, &Self::layout(header))?))
    }

    /// RLE data for all-zero planes of the given layout (black, or transparent in an alpha
    /// channel): the same bytes as [`Self::encode`] with RLE on zeros, without the zero buffer
    /// (one packed row repeated). For a placeholder a renderer replaces.
    pub fn blank_rle(header: &Header) -> Result<Self> {
        let layout = Self::layout(header);
        let rows = layout.rows()?;
        let row = crate::compression::packbits::encode_vec(&vec![0u8; layout.row_bytes()]);
        let count = row.len();
        let psb = header.version.is_psb();
        let mut data = Vec::new();
        let total = rows.checked_mul(count + if psb { 4 } else { 2 }).ok_or(crate::error::PsdError::LimitExceeded("merged image too large"))?;
        data.try_reserve_exact(total).map_err(|_| crate::error::PsdError::LimitExceeded("not enough memory for the merged image"))?;
        for _ in 0..rows {
            if psb {
                data.extend_from_slice(&u32::try_from(count).map_err(|_| crate::error::PsdError::LimitExceeded("RLE row too large"))?.to_be_bytes());
            } else {
                data.extend_from_slice(
                    &u16::try_from(count).map_err(|_| crate::error::PsdError::LimitExceeded("RLE row exceeds 65535 bytes (use PSB)"))?.to_be_bytes(),
                );
            }
        }
        for _ in 0..rows {
            data.extend_from_slice(&row);
        }
        Ok(ImageData { compression: Compression::Rle, data, stored: None })
    }

    /// Decodes all channels into planar big-endian samples.
    pub fn decode(&self, header: &Header) -> Result<Vec<u8>> {
        decode_planes(self.compression, &self.encoded()?, &Self::layout(header))
    }

    pub(crate) fn read(r: &mut Reader<'_>, header: &Header) -> Result<Self> {
        let compression = Compression::from_u16(r.u16()?);
        let data = r.peek_rest();
        validate_planes(compression, data, &Self::layout(header))?;
        let data = data.to_vec();
        r.skip(data.len())?;
        Ok(ImageData::new(compression, data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::{ColorMode, Version};

    #[test]
    fn blank_rle_is_rle_of_zeros() {
        for (version, depth, w) in [(Version::Psd, 8, 300), (Version::Psb, 8, 129), (Version::Psd, 16, 7), (Version::Psd, 1, 13), (Version::Psd, 8, 0)] {
            let header = Header::new(version, w, 5, 3, depth, if depth == 1 { ColorMode::Bitmap } else { ColorMode::Rgb });
            let zeros = vec![0u8; ImageData::layout(&header).decoded_len().unwrap()];
            let blank = ImageData::blank_rle(&header).unwrap();
            assert_eq!(blank, ImageData::encode(Compression::Rle, &zeros, &header).unwrap(), "{version:?} {depth} {w}");
            assert_eq!(blank.decode(&header).unwrap(), zeros);
        }
    }
}
