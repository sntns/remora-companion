use std::{fs::File, io};

use backhand::{
    v4::compressor::Compressor as BackhandCompressor, FilesystemCompressor, FilesystemReader,
    FilesystemWriter, NodeHeader,
};
use error_stack::Report;
use remora_squashfs::{
    adapter::{Error, InspectedEntry, Result, SquashfsAdapter},
    model::{BuildOptions, Compression, Entry, EntryKind, EntryMetadata},
};

fn map_compressor(compression: Compression) -> BackhandCompressor {
    match compression {
        Compression::Gzip => BackhandCompressor::Gzip,
        Compression::Xz => BackhandCompressor::Xz,
        Compression::Lz4 => BackhandCompressor::Lz4,
        Compression::Zstd => BackhandCompressor::Zstd,
    }
}

fn node_header(metadata: EntryMetadata) -> NodeHeader {
    NodeHeader::new(
        metadata.permissions,
        metadata.uid,
        metadata.gid,
        metadata.mtime,
    )
}

/// Build a squashfs image from already-resolved `entries` and write it to
/// `out`. Returns the number of bytes written.
fn write(
    entries: &[Entry],
    options: &BuildOptions,
    image_mtime: u32,
    root_owner: (u32, u32),
    mut out: File,
) -> std::result::Result<u64, Error> {
    let mut fsw = FilesystemWriter::default();
    fsw.set_block_size(options.block_size);
    // Known backhand 0.25 quirk: this does not reach the synthetic root
    // inode's own mtime (verified against `unsquashfs -ll`, root always
    // reads back as epoch 0) even though it does apply to every real entry
    // below. Harmless for our use case (only real files/dirs matter), but
    // it's a real mksquashfs-compatibility gap worth knowing about.
    fsw.set_time(image_mtime);
    fsw.set_root_mode(options.root_mode);
    fsw.set_root_uid(root_owner.0);
    fsw.set_root_gid(root_owner.1);

    let compressor = FilesystemCompressor::new(map_compressor(options.compression), None)?;
    fsw.set_compressor(compressor);

    // Directories must exist before anything can be pushed underneath them;
    // push shallowest-first so parents always precede their children.
    let mut dirs: Vec<&Entry> = entries
        .iter()
        .filter(|e| matches!(e.kind, EntryKind::Directory))
        .collect();
    dirs.sort_by_key(|e| e.path.components().count());
    for entry in dirs {
        fsw.push_dir_all(&entry.path, node_header(entry.metadata))?;
    }

    for entry in entries {
        match &entry.kind {
            EntryKind::Directory => {} // handled above
            EntryKind::File { source } => {
                let reader = File::open(source).map_err(|e| Error::OpenInput(source.clone(), e))?;
                fsw.push_file(reader, &entry.path, node_header(entry.metadata))?;
            }
            EntryKind::Symlink { target } => {
                fsw.push_symlink(
                    target.to_string_lossy().as_ref(),
                    &entry.path,
                    node_header(entry.metadata),
                )?;
            }
        }
    }

    let (_super_block, bytes_written) = fsw.write(&mut out)?;
    Ok(bytes_written)
}

fn inspect(input: File) -> std::result::Result<Vec<InspectedEntry>, Error> {
    let reader = io::BufReader::new(input);
    let fs = FilesystemReader::from_reader(reader)?;

    let entries = fs
        .files()
        .map(|node| {
            let kind = match &node.inner {
                backhand::InnerNode::Dir(_) => "dir",
                backhand::InnerNode::File(_) => "file",
                backhand::InnerNode::Symlink(_) => "symlink",
                backhand::InnerNode::CharacterDevice(_) => "char-device",
                backhand::InnerNode::BlockDevice(_) => "block-device",
                backhand::InnerNode::NamedPipe => "fifo",
                backhand::InnerNode::Socket => "socket",
            };
            InspectedEntry {
                path: node.fullpath.clone(),
                kind,
                permissions: node.header.permissions,
                uid: node.header.uid,
                gid: node.header.gid,
                mtime: node.header.mtime,
            }
        })
        .collect();

    Ok(entries)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SquashfsAdapterImpl;

impl SquashfsAdapter for SquashfsAdapterImpl {
    fn write(
        &self,
        entries: &[Entry],
        options: &BuildOptions,
        image_mtime: u32,
        root_owner: (u32, u32),
        out: File,
    ) -> Result<u64> {
        write(entries, options, image_mtime, root_owner, out).map_err(Report::new)
    }

    fn inspect(&self, input: File) -> Result<Vec<InspectedEntry>> {
        inspect(input).map_err(Report::new)
    }
}
