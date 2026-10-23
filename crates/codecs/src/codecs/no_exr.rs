//! OpenEXR without the `exr` feature (the web build leaves the `exr` crate out): files are still
//! recognised, and every entry point of `exr.rs` returns an "unsupported" error, never a panic.

use crate::Format;
use crate::error::CodecError;
use crate::fidelity::Plan;
use crate::image::{DeepImage, ExrPartInfo, Image};
use crate::options::{EncodeOptions, Limits};

const F: Format = Format::OpenExr;

/// The reason given when this build has no OpenEXR codec.
pub(crate) const NOT_IN_BUILD: &str = "OpenEXR support isn't included in this build of Capishot";

fn unsupported<T>() -> Result<T, CodecError> {
    Err(CodecError::unsupported(F, NOT_IN_BUILD))
}

pub(crate) fn info(_bytes: &[u8], _limits: &Limits) -> Result<Vec<ExrPartInfo>, CodecError> {
    unsupported()
}

pub(crate) fn decode(_bytes: &[u8], _limits: &Limits) -> Result<Image, CodecError> {
    unsupported()
}

pub(crate) fn decode_part(_bytes: &[u8], _index: usize, _limits: &Limits) -> Result<Image, CodecError> {
    unsupported()
}

pub(crate) fn decode_channels(_bytes: &[u8], _index: usize, _names: &[&str], _limits: &Limits) -> Result<Image, CodecError> {
    unsupported()
}

pub(crate) fn decode_deep(_bytes: &[u8], _limits: &Limits) -> Result<DeepImage, CodecError> {
    unsupported()
}

pub(crate) fn encode(_src: &Image, _plan: Plan, _opts: &EncodeOptions) -> Result<Vec<u8>, CodecError> {
    unsupported()
}

pub(crate) fn probe(_bytes: &[u8], _limits: &Limits) -> Result<(u32, u32), CodecError> {
    unsupported()
}
