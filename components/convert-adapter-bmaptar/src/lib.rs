//! A `.bmaptar`, as meta-remora's `remora-bmaptar.bbclass` builds it: a
//! plain GNU tar of `<name>.bmap` (`bmaptool create`'s) then `<name>.zst`
//! (the image, `zstd -3 --threads`), owned by root. Read in place, the
//! image decoded straight into a sparse raw file through its own `.bmap`;
//! written back the same way from a raw image, its `.bmap` made from the
//! file's holes, as bmaptool does.

mod bmap;
mod decode;
mod encode;
mod map;
mod service;

pub use service::BmaptarAdapterImpl;
