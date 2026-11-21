//! The platform-independent half of File > Scan document: the archive scanned pages are kept in, and the checks that tell the reader when a page went wrong. Talking to scanners is the `paperback-scan` crate's job, and recognizing text is the app's OCR engine's.

pub mod archive;
pub mod check;
mod common_han;
