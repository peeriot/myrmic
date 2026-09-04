use std::path::Path;

use bollard::{Docker, body_full, query_parameters::BuildImageOptionsBuilder};
use bytes::Bytes;
use futures::StreamExt;

use crate::docker::managed::ManagedContainer;

/// a thin wrapper around docker images identified by their tag
pub struct Image {
    image_tag: String,
}

impl Image {
    /// create image from a known reference
    pub fn new(image_tag: String) -> Self {
        Self { image_tag }
    }

    /// create image by building it from `dockerfile` with an in-memory tar build context
    /// containing the given `files` as `(host path, name in context)` pairs
    pub async fn build(
        docker: &Docker,
        image_tag: impl Into<String>,
        dockerfile: &Path,
        files: &[(&Path, &str)],
    ) -> Self {
        let image_tag = image_tag.into();
        let context = build_context(dockerfile, files);

        let build_opts = BuildImageOptionsBuilder::new()
            .dockerfile("Dockerfile")
            .t(&image_tag)
            .build();

        let docker_for_build = docker.clone();
        let mut stream =
            docker_for_build.build_image(build_opts, None, Some(body_full(Bytes::from(context))));
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

        Self { image_tag }
    }

    /// the image tag (e.g. `myrmic-e2e:latest`)
    pub fn tag(&self) -> &str {
        &self.image_tag
    }

    /// start the docker container with idle command
    pub async fn run_idle(&self, docker: Docker, name: &str) -> ManagedContainer {
        ManagedContainer::run(self, docker, &["sh", "-c", "tail -f /dev/null"], &[], name).await
    }

    /// start the docker container with specified command
    pub async fn run_command(
        &self,
        docker: Docker,
        command: &[&str],
        name: &str,
    ) -> ManagedContainer {
        ManagedContainer::run(self, docker, command, &[], name).await
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
