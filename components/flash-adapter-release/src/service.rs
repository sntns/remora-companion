use error_stack::ResultExt;
use remora_context::model::ContextOverride;
use remora_flash::{
    adapter::release::{ArtifactChunks, Error, ReleaseArtifactAdapter, Result},
    model::{DiskImage, ReleaseArtifact},
};
use remora_ota::{
    adapter::gateway,
    application::OtaService,
    model::{tags, Artifact},
};

/// The tags a release's images carry in their tag condition, as
/// meta-remora's CI publishes them (`board:<machine> && type:diskimage`,
/// `type:installer`...).
const TYPE_TAG: &str = "type:";
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
        image_type: &str,
    ) -> Result<Vec<DiskImage>> {
        let release_descriptor = self
            .ota
            .get_release(over, release)
            .await
            .change_context_lazy(|| Error::Release(release.to_owned()))?;
        Ok(images_of_type(release_descriptor.artifacts, image_type))
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

/// `artifacts` tagged `type:<image_type>`, by board then file name.
fn images_of_type(artifacts: Vec<Artifact>, image_type: &str) -> Vec<DiskImage> {
    let type_tag = format!("{TYPE_TAG}{image_type}");
    let mut images: Vec<_> = artifacts
        .into_iter()
        .filter_map(|artifact| {
            let tags = tags(&artifact.tag_condition);
            tags.contains(&type_tag.as_str()).then(|| DiskImage {
                boards: tags
                    .iter()
                    .filter_map(|tag| tag.strip_prefix(BOARD_TAG))
                    .map(str::to_owned)
                    .collect(),
                file_name: artifact.file_name,
                size: artifact.content_length,
                tag_condition: artifact.tag_condition.clone(),
            })
        })
        .collect();
    images.sort_by(|a, b| a.boards.cmp(&b.boards).then(a.file_name.cmp(&b.file_name)));
    images
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

    fn artifact(file_name: &str, tag_condition: &str) -> Artifact {
        Artifact {
            file_name: file_name.into(),
            content_type: "application/octet-stream".into(),
            content_length: 42,
            checksum_sha256: String::new(),
            tag_condition: tag_condition.into(),
        }
    }

    fn release() -> Vec<Artifact> {
        vec![
            artifact("bundle-f3apl.raucb", "board:f3apl && type:rauc"),
            artifact(
                "installer-f3apl.wic.bmaptar",
                "board:f3apl && type:installer",
            ),
            artifact("disk-rp5.wic.bmaptar", "board:rp5 && type:diskimage"),
            artifact("disk-f3apl.wic.bmaptar", "board:f3apl && type:diskimage"),
        ]
    }

    fn names(images: &[DiskImage]) -> Vec<(&str, Vec<String>)> {
        images
            .iter()
            .map(|image| (image.file_name.as_str(), image.boards.clone()))
            .collect()
    }

    #[test]
    fn selects_the_images_of_the_type_asked_for() {
        assert_eq!(
            names(&images_of_type(release(), "diskimage")),
            [
                ("disk-f3apl.wic.bmaptar", vec!["f3apl".to_owned()]),
                ("disk-rp5.wic.bmaptar", vec!["rp5".to_owned()]),
            ]
        );
        assert_eq!(
            names(&images_of_type(release(), "installer")),
            [("installer-f3apl.wic.bmaptar", vec!["f3apl".to_owned()])]
        );
        assert!(images_of_type(release(), "nothing").is_empty());
    }

    #[test]
    fn a_type_is_a_whole_tag_not_a_prefix() {
        assert!(images_of_type(release(), "disk").is_empty());
        assert!(images_of_type(release(), "installer-f3apl").is_empty());
    }
}
