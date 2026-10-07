//! Turns an image file into the bytes to flash, through `remora-unpack`: a
//! `.bmaptar` bundle (an uncompressed tar of the image and its `.bmap`, as
//! meta-remora builds it) is read in place, without extracting it; the
//! image itself, bundled or not, is decompressed on the fly when it's bzip2
//! (on every core), gzip or zstd.

mod service;

pub use service::ImageSourceAdapterImpl;
