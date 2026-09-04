use std::io::Write;
use std::path::PathBuf;
use std::str::FromStr;

use anyhow::Context as _;
use human_bytes::human_bytes;

use object_store::ObjectStoreExt as _;
use object_store::aws::{AmazonS3, AmazonS3Builder};

use db_client::v1::models;
use db_client::v1::models::replication::{Snapshot, read_snapshot, write_snapshot};

use crate::args::Ctx;

mod monitor;
mod status;

#[derive(clap::Parser)]
pub struct Database {
    #[clap(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Subcommand)]
pub enum Cmd {
    Export(Export),
    Import(Import),
    Monitor(monitor::Monitor),
    Status(status::Status),
}

#[derive(clap::Parser)]
pub struct Export {
    scope: String,

    /// Destination URI: a path, `file://...`, or `s3://bucket/key`.
    ///
    /// For `s3://` targets, credentials are read from the standard AWS
    /// environment variables (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
    /// `AWS_SESSION_TOKEN`). The bucket must already exist.
    #[clap(long)]
    target: Location,

    #[clap(flatten)]
    s3: S3Options,
}

#[derive(clap::Parser)]
pub struct Import {
    /// Scope to restore into, should be provided as `namespace/database/schema`.
    ///
    /// Note: snapshots record the scope they were taken from, so this is just used to override the snapshot's recorded scope.
    scope: Option<String>,

    /// Source URI: a path, `file://...`, or `s3://bucket/key`.
    ///
    /// For `s3://` sources, credentials are read from the standard AWS
    /// environment variables (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
    /// `AWS_SESSION_TOKEN`).
    #[clap(long)]
    source: Location,

    #[clap(flatten)]
    s3: S3Options,
}

#[derive(clap::Args)]
pub struct S3Options {
    /// AWS region (s3 targets only).
    #[clap(long)]
    region: Option<String>,

    /// Override the S3 endpoint, e.g. for MinIO/LocalStack (s3 targets only).
    #[clap(long)]
    endpoint: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Location {
    File(PathBuf),
    S3 { bucket: String, key: String },
}

impl FromStr for Location {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = s.strip_prefix("s3://") {
            let (bucket, key) = rest
                .split_once('/')
                .ok_or_else(|| anyhow::anyhow!("s3 URI must be `s3://bucket/key`: {s}"))?;
            if bucket.is_empty() || key.is_empty() {
                anyhow::bail!("s3 URI must have a non-empty bucket and key: {s}");
            }
            Ok(Self::S3 {
                bucket: bucket.to_owned(),
                key: key.to_owned(),
            })
        } else if let Some(rest) = s.strip_prefix("file://") {
            // Tolerate `file:///abs/path` and `file://localhost/abs/path` — strip the authority.
            let path = rest.strip_prefix("localhost").unwrap_or(rest);
            Ok(Self::File(PathBuf::from(path)))
        } else {
            Ok(Self::File(PathBuf::from(s)))
        }
    }
}

pub async fn handle(ctx: Ctx, cmd: Database) -> anyhow::Result<()> {
    match cmd.cmd {
        Cmd::Export(export) => handle_export(ctx, export).await,
        Cmd::Import(import) => handle_import(ctx, import).await,
        Cmd::Monitor(monitor) => monitor::handle(ctx, monitor).await,
        Cmd::Status(status) => status::handle(ctx, status).await,
    }
}

async fn handle_export(ctx: Ctx, export: Export) -> anyhow::Result<()> {
    let Export { scope, target, s3 } = export;

    let (namespace, database, schema) =
        crate::split!(&scope, '/', '/').map_err(anyhow::Error::msg)?;
    let scope = models::Scope::new(namespace, database, schema);

    let session = ctx.session().await?;
    let db = db_client::v1::Client::new(&session);

    let snapshot = db
        .read_tx_in(scope.clone(), {
            let scope = scope.clone();
            async move |c, tx_id| {
                c.send(models::scope_backup::Request {
                    id: tx_id,
                    op: models::scope_backup::Op { scope },
                })
                .await
            }
        })
        .await
        .map_err(|err| anyhow::anyhow!("unable to communicate with db: {}", err))?
        .map_err(|err| anyhow::anyhow!("unable to request backup: {}", err.message))?
        .snapshot;

    match target {
        Location::File(path) => {
            let file = std::fs::File::create(&path)
                .with_context(|| format!("unable to create snapshot file {}", path.display()))?;
            let mut writer = std::io::BufWriter::new(file);
            write_snapshot(&mut writer, &scope, &snapshot)?;
            writer
                .flush()
                .with_context(|| format!("unable to flush snapshot file {}", path.display()))?;

            let size = std::fs::metadata(&path).map_or(0, |m| m.len());
            #[allow(clippy::cast_precision_loss)]
            let size = human_bytes(size as f64);
            crate::info!(
                ctx,
                "exported {scope} ({} chunk(s), {size}) to {}",
                snapshot.len(),
                path.display()
            );
            Ok(())
        }
        Location::S3 { bucket, key } => {
            let S3Options { region, endpoint } = s3;
            let store = build_s3_store(&bucket, region, endpoint)?;

            let mut buf = Vec::new();
            write_snapshot(&mut buf, &scope, &snapshot)?;
            #[allow(clippy::cast_precision_loss)]
            let size = human_bytes(buf.len() as f64);
            let chunk_count = snapshot.len();

            let path = object_store::path::Path::from(key.as_str());
            store
                .put(&path, buf.into())
                .await
                .with_context(|| format!("unable to upload snapshot to s3://{bucket}/{key}"))?;

            crate::info!(
                ctx,
                "exported {scope} ({chunk_count} chunk(s), {size}) to s3://{bucket}/{key}"
            );
            Ok(())
        }
    }
}

async fn handle_import(ctx: Ctx, import: Import) -> anyhow::Result<()> {
    let Import { scope, source, s3 } = import;

    let requested_scope = scope
        .map(|scope| {
            let (namespace, database, schema) =
                crate::split!(&scope, '/', '/').map_err(anyhow::Error::msg)?;
            anyhow::Ok(models::Scope::new(namespace, database, schema))
        })
        .transpose()?;

    let session = ctx.session().await?;
    let db = db_client::v1::Client::new(&session);

    match source {
        Location::File(path) => {
            let file = std::fs::File::open(&path)
                .with_context(|| format!("unable to open snapshot file {}", path.display()))?;
            let mut reader = std::io::BufReader::new(file);
            let (header, snapshot) = read_snapshot(&mut reader)
                .with_context(|| format!("unable to read snapshot file {}", path.display()))?;

            restore_snapshot(
                ctx,
                &db,
                requested_scope,
                header.scope,
                snapshot,
                &path.display().to_string(),
            )
            .await
        }
        Location::S3 { bucket, key } => {
            let S3Options { region, endpoint } = s3;
            let store = build_s3_store(&bucket, region, endpoint)?;

            let path = object_store::path::Path::from(key.as_str());
            let bytes = store
                .get(&path)
                .await
                .with_context(|| format!("unable to fetch snapshot from s3://{bucket}/{key}"))?
                .bytes()
                .await
                .with_context(|| {
                    format!("unable to read snapshot body from s3://{bucket}/{key}")
                })?;

            let (header, snapshot) = read_snapshot(&mut bytes.as_ref())
                .with_context(|| format!("unable to decode snapshot from s3://{bucket}/{key}"))?;

            restore_snapshot(
                ctx,
                &db,
                requested_scope,
                header.scope,
                snapshot,
                &format!("s3://{bucket}/{key}"),
            )
            .await
        }
    }
}

/// Resolve the region for `SigV4` signing
fn resolve_region(explicit: Option<String>) -> String {
    if let Some(region) = explicit {
        return region;
    }

    // `MinIO` ignores the value but rejects a request signed with none, so we still need to add one.
    std::env::var("AWS_REGION")
        .ok()
        .or_else(|| std::env::var("AWS_DEFAULT_REGION").ok())
        .unwrap_or_else(|| "us-east-1".to_owned())
}

/// Build an S3-compatible store for the given bucket. Credentials come from the
/// environment (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
/// `AWS_SESSION_TOKEN`); `--region` / `--endpoint` override the defaults. A
/// custom `--endpoint` (MinIO/LocalStack) keeps the default path-style
/// addressing.
fn build_s3_store(
    bucket: &str,
    region: Option<String>,
    endpoint: Option<String>,
) -> anyhow::Result<AmazonS3> {
    let region = resolve_region(region);

    let mut builder = AmazonS3Builder::from_env()
        .with_bucket_name(bucket)
        .with_region(region);

    if let Some(endpoint) = endpoint {
        if endpoint.starts_with("http://") {
            builder = builder.with_allow_http(true);
        }
        builder = builder.with_endpoint(endpoint);
    }

    builder
        .build()
        .context("unable to build S3 client (check credentials and region)")
}

/// Restore a decoded snapshot into the db. Shared by the file and S3 import
/// paths: reconciles the target scope, runs the restore transaction, and prints
/// the summary. `origin` is the human-readable source (path or `s3://…`).
async fn restore_snapshot(
    ctx: Ctx,
    db: &db_client::v1::Client,
    requested_scope: Option<models::Scope>,
    snapshot_scope: models::Scope,
    snapshot: Snapshot,
    origin: &str,
) -> anyhow::Result<()> {
    let scope = match requested_scope {
        Some(scope) => {
            crate::warn!(
                ctx,
                "snapshot was taken from scope {} but restoring into {}",
                snapshot_scope,
                scope
            );
            scope
        }
        None => snapshot_scope,
    };

    let chunk_count = snapshot.len();
    let entries: usize = snapshot.iter().map(|c| c.entries.len()).sum();

    db.write_tx_in(scope.clone(), {
        let scope = scope.clone();
        async move |c, tx_id| {
            c.send(models::scope_restore::Request {
                id: tx_id,
                op: models::scope_restore::Op { scope, snapshot },
            })
            .await
        }
    })
    .await
    .map_err(|err| anyhow::anyhow!("unable to communicate with db: {}", err))?
    .map_err(|err| anyhow::anyhow!("unable to request restore: {}", err.message))?;

    crate::info!(
        ctx,
        "imported {} ({} chunk(s), {} entries) from {}",
        scope,
        chunk_count,
        entries,
        origin
    );
    Ok(())
}
