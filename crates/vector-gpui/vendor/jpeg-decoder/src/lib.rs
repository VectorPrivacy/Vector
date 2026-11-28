//! A JPEG decoder (jpeg-decoder 0.3.2, reworked for scaled decodes; see README.md).
//!
//! ```
//! # fn run(bytes: &[u8]) -> Result<(), jpeg_decoder::Error> {
//! let mut decoder = jpeg_decoder::Decoder::new(bytes);
//! decoder.read_info()?;
//! decoder.scale(200, 200)?;
//! decoder.decode_rows(|y, row| { let _ = (y, row); })?;
//! # Ok(()) }
//! ```

#![deny(missing_docs)]
#![deny(unsafe_code)]
#![cfg_attr(feature = "platform_independent", forbid(unsafe_code))]

extern crate alloc;
extern crate core;

pub use decoder::{ColorTransform, Decoder, ImageInfo, PixelFormat};
pub use error::{Error, UnsupportedFeature};
pub use parser::CodingProcess;

use std::io;

#[cfg(not(feature = "platform_independent"))]
mod arch;
mod decoder;
mod error;
mod huffman;
mod idct;
mod marker;
mod parser;
mod upsampler;
mod worker;

fn read_u8<R: io::Read>(reader: &mut R) -> io::Result<u8> {
    let mut buf = [0];
    reader.read_exact(&mut buf)?;
    Ok(buf[0])
}

fn read_u16_from_be<R: io::Read>(reader: &mut R) -> io::Result<u16> {
    let mut buf = [0, 0];
    reader.read_exact(&mut buf)?;
    Ok(u16::from_be_bytes(buf))
}
