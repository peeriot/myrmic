# myrmic database import

## Name
`myrmic database import` - Restore a snapshot into a database scope

Aliases: `db import`

## Synopsis
```
myrmic database import [OPTIONS] [SCOPE]
```

## Description
Restores a snapshot into a database scope. The snapshot can be read from:

- a local file path.
- a file URI.
- an S3 URI.

A `SCOPE` is a way to organize and split the data space, it also identifies a specific dataset in the distributed swarm storage, should be provided in this format `namespace/database/schema`.

> **Note:** Snapshots record the scope they were exported from, So `SCOPE` here is a way to override it, it is optional and if not passed the one recorded in the snapshot is used

For S3 sources, credentials are read from environment variables: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, and optionally `AWS_SESSION_TOKEN`. The bucket must exist before running this command.

## Options
`--source URI`

Specifies the snapshot source. Required. Accepts:

- a local file path.
- a file URI ``file://...``.
- an S3 URI (`s3://bucket/key`).

`--region REGION`

AWS region. S3 only. Falls back to `AWS_REGION`, `AWS_DEFAULT_REGION` environment variables, if not set then `us-east-1`.

`--endpoint URL`

Overrides the S3 API endpoint. Use for S3-compatible services such as MinIO or LocalStack. S3 only.

`-v` / `--verbose`

Verbose mode, shows more detail about what the command is doing. Useful for debugging. Can be set to `-vv` for even more detail.

`-h`, `--help`

Prints help information.

## Examples
1. Restore a snapshot file URI into a different scope than the one it was exported from:

```bash
myrmic database import my-namespace/my-database/my-schema --source file:///var/backups/snapshot
```

2. Restore a snapshot from AWS S3 with an explicit region:

```bash
myrmic database import --source s3://my-bucket/my-snapshot --region eu-west-1
```

6. Restore a snapshot from a local MinIO instance:

```bash
myrmic database import --source s3://my-bucket/my-snapshot --endpoint http://localhost:9000
```
