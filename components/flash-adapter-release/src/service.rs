use error_stack::ResultExt;
use remora_context::model::ContextOverride;
use remora_flash::{
    adapter::release::{ArtifactChunks, Error, ReleaseArtifactAdapter, Result},
    model::{DiskImage, ReleaseArtifact},
};
use remora_ota::{adapter::gateway, application::OtaService};

/// The tag a release's disk images carry in their tag condition, as
/// meta-remora's CI publishes them (`board:<machine> && type:diskimage`).
const DISK_IMAGE_TAG: &str = "type:diskimage";
const BOARD_TAG: &str = "board:";

pub struct ReleaseArtifactAdapterImpl {
    ota: OtaService,
}

impl ReleaseArtifactAdapterImpl {
    pub fn new(ota: OtaService) -> Self {
        Self { ota }
    }
}

#[async_trait::async_trait]
impl ReleaseArtifactAdapter for ReleaseArtifactAdapterImpl {
    async fn disk_images(
        &self,
        over: Option<&ContextOverride>,
        release: &str,
    ) -> Result<Vec<DiskImage>> {
        let release_descriptor = self
            .ota
            .get_release(over, release)
            .await
            .change_context_lazy(|| Error::Release(release.to_owned()))?;
        let mut images: Vec<_> = release_descriptor
            .artifacts
            .into_iter()
            .filter_map(|artifact| {
                let tags = tags(&artifact.tag_condition);
                tags.contains(&DISK_IMAGE_TAG).then(|| DiskImage {
                    boards: tags
                        .iter()
                        .filter_map(|tag| tag.strip_prefix(BOARD_TAG))
                        .map(str::to_owned)
                        .collect(),
                    file_name: artifact.file_name,
                    size: artifact.content_length,
                })
            })
            .collect();
        images.sort_by(|a, b| a.boards.cmp(&b.boards).then(a.file_name.cmp(&b.file_name)));
        Ok(images)
    }

    async fn download(
        &self,
        artifact: &ReleaseArtifact,
        offset: u64,
    ) -> Result<Box<dyn ArtifactChunks>> {
        let chunks = self
            .ota
            .download(
                artifact.over.as_ref(),
                &artifact.release,
                &artifact.file_name,
                offset,
            )
            .await
            .change_context_lazy(|| Error::Download(artifact.to_string()))?;
        Ok(Box::new(OtaChunks {
            chunks,
            name: artifact.to_string(),
        }))
    }
}

/// The tags a tag condition names (`board:rp5 && (type:diskimage)` names
/// `board:rp5` and `type:diskimage`), whatever the operators around them.
fn tags(condition: &str) -> Vec<&str> {
    condition
        .split(|c: char| !(c.is_alphanumeric() || matches!(c, ':' | '_' | '-' | '.' | '/')))
        .filter(|tag| tag.contains(':'))
        .collect()
}

struct OtaChunks {
    chunks: Box<dyn gateway::ArtifactChunks>,
    name: String,
}

#[async_trait::async_trait]
impl ArtifactChunks for OtaChunks {
    async fn next(&mut self) -> Result<Option<Vec<u8>>> {
        self.chunks
            .next()
            .await
            .change_context_lazy(|| Error::Download(self.name.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_read_through_the_operators() {
        assert_eq!(
            tags("board:f3apl && type:diskimage"),
            ["board:f3apl", "type:diskimage"]
        );
        assert_eq!(
            tags("(board:rp5 || board:hdc)&&type:diskimage"),
            ["board:rp5", "board:hdc", "type:diskimage"]
        );
        assert!(tags("").is_empty());
    }
}
