//! Docker images built by the framework.

use std::path::Path;

use bollard::{body_full, query_parameters::BuildImageOptionsBuilder};
use bytes::Bytes;
use futures::StreamExt;

use crate::docker::DockerDaemon;

/// A docker image the daemon has: only [`Self::find`] and [`Self::build`] create one.
pub struct Image {
    /// the image's reference, e.g. `myrmic-swarm:0123456789abcdef`
    tag: String,
}

impl Image {
    /// the image `tag`, or `None` if the daemon doesn't have it
    pub async fn find(docker: &DockerDaemon, tag: impl Into<String>) -> Option<Self> {
        let tag = tag.into();
        match docker.inspect_image(&tag).await {
            Ok(_) => Some(Self { tag }),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => None,
            Err(err) => panic!("failed to inspect image `{tag}`: {err}"),
        }
    }

    /// Build the image `tag` from `dockerfile` with an in-memory tar build context containing the
    /// given `files` as `(host path, name in context)` pairs.
    pub async fn build(
        docker: &DockerDaemon,
        tag: impl Into<String>,
        dockerfile: &Path,
        files: &[(&Path, &str)],
    ) -> Self {
        let tag = tag.into();
        let context = build_context(dockerfile, files);
        let options = BuildImageOptionsBuilder::new()
            .dockerfile("Dockerfile")
            .t(&tag)
            .build();
        let mut stream = docker.build_image(options, None, Some(body_full(Bytes::from(context))));
        while let Some(result) = stream.next().await {
            let info = result.unwrap();
            if let Some(error) = info
                .error_detail
                .as_ref()
                .and_then(|detail| detail.message.as_deref())
            {
                panic!("Docker build failed: {error}");
            }
        }
        Self { tag }
    }

    /// the image's reference, e.g. `myrmic-swarm:0123456789abcdef`
    pub fn tag(&self) -> &str {
        &self.tag
    }
}

fn build_context(dockerfile: &Path, files: &[(&Path, &str)]) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    append_file(&mut archive, dockerfile, "Dockerfile");
    for (path, name) in files {
        append_file(&mut archive, path, name);
    }
    archive.into_inner().unwrap()
}

fn append_file(archive: &mut tar::Builder<Vec<u8>>, path: &Path, name: &str) {
    let data = std::fs::read(path).unwrap();
    let mut header = tar::Header::new_ustar();
    header.set_size(
        data.len()
            .try_into()
            .expect("file too large for tar header"),
    );
    header.set_mode(0o755);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    archive
        .append_data(&mut header, name, data.as_slice())
        .unwrap();
}
