# myrmic database export

## Name
`myrmic database export` - Export a database scope to a snapshot

Aliases: `db export`

## Synopsis
```
myrmic database export [OPTIONS] [SCOPE]
```

## Description
Exports a database `SCOPE` to a snapshot. The snapshot can be written to:

- a local file path.
- a file URI.
- an S3 URI.

A [SCOPE] is a way to organize and split the data space, it also identifies a specific dataset in the distributed swarm storage, should be provided in this format `namespace/database/schema`.

For S3 targets, credentials are read from the `AWS_ACCESS_KEY_ID` and `AWS_SECRET_ACCESS_KEY` environment variables, with optional support for `AWS_SESSION_TOKEN`. Despite their names, these variables can also provide credentials for non-AWS, S3-compatible services, such as MinIO, Google Cloud Storage, and IBM Cloud Storage. The target bucket must exist before you run this command.

## Options
`--target URI`

Specifies the snapshot destination. Required. Accepts:

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
1. Export a scope using a file URI:

```bash
myrmic database export my-namespace/my-database/my-schema --target file:///var/backups/snapshot
```

2. Export a scope to AWS S3 with an explicit region:

```bash
myrmic database export my-namespace/my-database/my-schema --target s3://my-bucket/my-snapshot --region eu-west-1
```

5. Export a scope to a local MinIO instance:

```bash
myrmic database export my-namespace/my-database/my-schema --target s3://my-bucket/my-snapshot --endpoint http://localhost:9000
```
