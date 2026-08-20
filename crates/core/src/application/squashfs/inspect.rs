use std::{fs, path::Path};

use crate::adapter::squashfs::{self, InspectedEntry};

use super::error::Error;

pub fn inspect(image: &Path) -> Result<Vec<InspectedEntry>, Error> {
    let file = fs::File::open(image).map_err(|source| Error::OpenInput {
        path: image.to_path_buf(),
        source,
    })?;
    Ok(squashfs::inspect(file)?)
}
