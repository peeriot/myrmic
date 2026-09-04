use db_client::v1::models;
use db_client::v1::models::FieldValue;

use std::fmt::Write as _;
use zenoh::session::ZenohId;

#[cfg(test)]
mod tests;

// Just enough of a config to start communication
const DEFAULT_CONFIG: &str = r#"
local z = import "zenoh.libsonnet";

z.peer()
"#;

pub struct Context {
    pub tx_id: Option<models::TxId>,
    pub scope: models::Scope,
    session: zenoh::Session,
    client: db_client::v1::Client,

    _spawned: swarm::spawn::Spawned,
}

pub enum DbResponse {
    DbInfo(Vec<models::db_info::Response>),
    TxBegin(models::tx_begin::Response),
    TxCommit(models::tx_commit::Response),
    TxRollback(models::tx_rollback::Response),
    TbInsert(models::tb_insert::Response),
    TbGet(models::tb_get::Response),
    TbDelete(models::tb_delete::Response),
    TbList(models::tb_list::Response),
    KeyPut(models::key_put::Response),
    KeyGet(models::key_get::Response),
    KeyDelete(models::key_delete::Response),
    BlobStore(models::blob_store::Response),
    BlobLink(models::blob_link::Response),
    BlobUnlink(models::blob_unlink::Response),
    BlobMove(models::blob_move::Response),
    BlobResolve(models::blob_resolve::Response),
    PathResolve(models::path_resolve::Response),
    PathsList(models::paths_list::Response),
    TsPublish(models::ts_publish::Response),
    TsFind(models::ts_find::Response),
}

impl Context {
    pub async fn new() -> Self {
        Self::with_config(DEFAULT_CONFIG).await
    }

    pub async fn with_config(config: impl AsRef<str>) -> Self {
        let swarm = swarm::Swarm::parse(config).expect("Config should be valid");
        Self::from_swarm(swarm).await
    }

    pub async fn from_swarm(swarm: swarm::Swarm) -> Self {
        let spawned = swarm.wait_in_place().await.expect("Unable to spawn swarm");

        let session = spawned.session().clone();
        let client = db_client::v1::Client::new(&session);

        let scope = models::Scope::default();

        Self {
            tx_id: None,
            scope,
            client,
            session,
            _spawned: spawned,
        }
    }

    pub async fn execute_line(&mut self, line: &str) -> Option<DbResponse> {
        let line = line.trim_start();

        let (cmd, args) = line.split_once(' ').unwrap_or((line, ""));
        if cmd == "help" {
            println!(
                "help\nbegin\ncommit\nmark\nreset\nkey_put\nkey_get\nkey_delete\nblob_store\nblob_link\nblob_unlink\nblob_move\nblob_resolve\npath_resolve\npaths_list\nts_publish\nts_find"
            );
            return None;
        }

        let args = args.trim_start();

        let args = args
            .split_ascii_whitespace()
            .filter_map(|segment| {
                let segment = segment.trim();
                (!segment.is_empty()).then_some(segment)
            })
            .collect::<Vec<_>>();

        let args = args.as_slice();

        self.execute(cmd, args).await
    }

    #[allow(clippy::too_many_lines)] // fitemeirl
    pub async fn execute(&mut self, cmd: &str, args: &[&str]) -> Option<DbResponse> {
        let Self {
            tx_id,
            scope,
            client,
            session,
            _spawned: _,
        } = self;

        match cmd {
            "info" => {
                let result = client
                    .send(models::db_info::Request {})
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => {
                        for response in &response {
                            let id = ZenohId::try_from(response.id.as_slice())
                                .expect("unable to convert id");
                            println!("\t{}", id);
                        }

                        Some(DbResponse::DbInfo(response))
                    }
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "begin" => {
                if tx_id.is_some() {
                    println!("Transaction already started");
                    return None;
                }

                let request = match *args {
                    ["routed"] => models::tx_begin::Request::routed(scope.clone()),
                    _ => models::tx_begin::Request::default(),
                };

                let result = client.send(request).await.expect("unable to communicate");

                match result {
                    Ok(response) => {
                        self.tx_id = Some(response.id);
                        Some(DbResponse::TxBegin(response))
                    }
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "commit" => {
                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let result = client
                    .send(models::tx_commit::Request { id: *id })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => {
                        self.tx_id = None;
                        Some(DbResponse::TxCommit(response))
                    }
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "rollback" => {
                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let result = client
                    .send(models::tx_rollback::Request { id: *id })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => {
                        self.tx_id = None;
                        Some(DbResponse::TxRollback(response))
                    }
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "scope" => {
                let [namespace, database, schema] = *args else {
                    println!("{{namespace}} {{database}} {{schema}}");
                    return None;
                };

                self.scope = models::Scope {
                    namespace: String::from(namespace),
                    database: String::from(database),
                    schema: String::from(schema),
                };

                None
            }
            "paths_list" => {
                let Some(id) = *tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let result = client
                    .send(models::paths_list::Request {
                        id,
                        op: models::paths_list::Op {
                            scope: scope.clone(),
                            limit: None,
                        },
                    })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => {
                        println!("paths:");
                        for path in response.paths.iter() {
                            println!("{}", path);
                        }
                        Some(DbResponse::PathsList(response))
                    }
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "key_put" => {
                let [key, value] = *args else {
                    println!("{{key}} {{value}}");
                    return None;
                };

                let Some(id) = *tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let result = client
                    .send(models::key_put::Request {
                        id,
                        op: models::key_put::Op {
                            scope: scope.clone(),
                            key: String::from(key),
                            value: value.as_bytes().to_vec(),
                        },
                    })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => Some(DbResponse::KeyPut(response)),
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "key_get" => {
                let [key] = *args else {
                    println!("{{key}}");
                    return None;
                };

                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let result = client
                    .send(models::key_get::Request {
                        id: *id,
                        op: models::key_get::Op {
                            scope: scope.clone(),
                            key: String::from(key),
                        },
                    })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => {
                        let value = response.value.as_deref().map(String::from_utf8_lossy);
                        if let Some(value) = value {
                            println!("{} => {}", key, value);
                        } else {
                            println!("{} => [absent]", key);
                        }
                        Some(DbResponse::KeyGet(response))
                    }
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "key_delete" => {
                let [key] = *args else {
                    println!("{{key}}");
                    return None;
                };

                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let result = client
                    .send(models::key_delete::Request {
                        id: *id,
                        op: models::key_delete::Op {
                            scope: scope.clone(),
                            key: String::from(key),
                        },
                    })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => Some(DbResponse::KeyDelete(response)),
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "blob_store" => {
                let [blob] = *args else {
                    println!("{{blob}}");
                    println!("prefix with `hex:` to send hex bytes");
                    println!("prefix with `./` to load bytes from a file path");
                    return None;
                };

                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let blob = if let Some(value) = blob.strip_prefix("hex:") {
                    match hex::decode(value) {
                        Ok(value) => value,
                        Err(_err) => {
                            println!("Invalid hex blob payload");
                            return None;
                        }
                    }
                } else if blob.starts_with("./") {
                    match std::fs::read(blob) {
                        Ok(value) => value,
                        Err(err) => {
                            println!("Unable to read blob file '{}': {}", blob, err);
                            return None;
                        }
                    }
                } else {
                    blob.as_bytes().to_vec()
                };

                let result = client
                    .send(models::blob_store::Request {
                        id: *id,
                        op: models::blob_store::Op {
                            scope: scope.clone(),
                            blob,
                        },
                    })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => {
                        println!(
                            "{} / {}/{}/{}",
                            response.blob_id.hash.to_hex(),
                            response.blob_id.scope.namespace,
                            response.blob_id.scope.database,
                            response.blob_id.scope.schema
                        );

                        Some(DbResponse::BlobStore(response))
                    }
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "blob_link" => {
                let [hash, path] = *args else {
                    println!("{{sha2_hex_hash}} {{path}}");
                    return None;
                };

                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let hash = models::BlobHash::from_hex(hash)?;
                let blob_id = models::BlobId {
                    scope: scope.clone(),
                    hash,
                };

                let result = client
                    .send(models::blob_link::Request {
                        id: *id,
                        op: models::blob_link::Op {
                            blob_id,
                            path: String::from(path),
                        },
                    })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => Some(DbResponse::BlobLink(response)),
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "blob_unlink" => {
                let [path] = *args else {
                    println!("{{path}}");
                    return None;
                };

                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let result = client
                    .send(models::blob_unlink::Request {
                        id: *id,
                        op: models::blob_unlink::Op {
                            scope: scope.clone(),
                            path: String::from(path),
                        },
                    })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => Some(DbResponse::BlobUnlink(response)),
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "blob_move" => {
                let [old_path, new_path] = *args else {
                    println!("{{old_path}} {{new_path}}");
                    return None;
                };

                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let result = client
                    .send(models::blob_move::Request {
                        id: *id,
                        op: models::blob_move::Op {
                            scope: scope.clone(),
                            old_path: String::from(old_path),
                            new_path: String::from(new_path),
                        },
                    })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => Some(DbResponse::BlobMove(response)),
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "blob_resolve" => {
                let [hash] = *args else {
                    println!("{{sha2_hex_hash}}");
                    return None;
                };

                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let hash = models::BlobHash::from_hex(hash)?;
                let blob_id = models::BlobId {
                    scope: scope.clone(),
                    hash,
                };

                let result = client
                    .send(models::blob_resolve::Request {
                        id: *id,
                        op: models::blob_resolve::Op {
                            blob_id,
                            range: None,
                        },
                    })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => {
                        match response
                            .blob
                            .as_ref()
                            .map(|models::BlobResponse { blob, .. }| blob)
                        {
                            Some(blob) => {
                                println!("blob[{}]: {}", blob.len(), String::from_utf8_lossy(blob));
                            }
                            None => println!("blob: [absent]"),
                        }

                        Some(DbResponse::BlobResolve(response))
                    }
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "path_resolve" => {
                let [path] = *args else {
                    println!("{{path}}");
                    return None;
                };

                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let result = client
                    .send(models::path_resolve::Request {
                        id: *id,
                        op: models::path_resolve::Op {
                            scope: scope.clone(),
                            path: String::from(path),
                            range: None,
                        },
                    })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => {
                        match response
                            .blob
                            .as_ref()
                            .map(|models::BlobResponse { blob, .. }| blob)
                        {
                            Some(blob) => {
                                println!("blob[{}]: {}", blob.len(), String::from_utf8_lossy(blob));
                            }
                            None => println!("blob: [absent]"),
                        }

                        Some(DbResponse::PathResolve(response))
                    }
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }

            "tb_list" => {
                let (table, limit) = match *args {
                    [table] => (table, None),
                    [table, limit] => (table, Some(limit.parse().unwrap())),
                    _ => {
                        println!("{{table}} [{{limit}}]");
                        return None;
                    }
                };

                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let result = client
                    .send(models::tb_list::Request {
                        id: *id,
                        op: models::tb_list::Op {
                            scope: scope.clone(),
                            table: String::from(table),
                            limit,
                            cursor: None,
                            order: None,
                        },
                    })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => {
                        for (id, value) in &response.entities {
                            let eid = hex::encode(id);
                            println!("{} => {}", eid, String::from_utf8_lossy(value));
                        }

                        Some(DbResponse::TbList(response))
                    }
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }

            "tb_delete" => {
                let (table, eid) = match *args {
                    [table, id] => (table, id.as_bytes()),
                    _ => {
                        println!("{{table}} {{id}}");
                        return None;
                    }
                };

                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let eid = hex::decode(eid).unwrap_or_else(|_err| eid.to_vec());

                let result = client
                    .send(models::tb_delete::Request {
                        id: *id,
                        op: models::tb_delete::Op {
                            scope: scope.clone(),
                            table: String::from(table),
                            eid,
                        },
                    })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => Some(DbResponse::TbDelete(response)),
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }

            "tb_get" => {
                let (table, eid) = match *args {
                    [table, id] => (table, id.as_bytes()),
                    _ => {
                        println!("{{table}} {{id}}");
                        return None;
                    }
                };

                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let eid = hex::decode(eid).unwrap_or_else(|_err| eid.to_vec());

                let result = client
                    .send(models::tb_get::Request {
                        id: *id,
                        op: models::tb_get::Op {
                            scope: scope.clone(),
                            table: String::from(table),
                            eid,
                        },
                    })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => Some(DbResponse::TbGet(response)),
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }

            "tb_insert" => {
                let (table, eid, data) = match *args {
                    [table] => (table, None, b"{\"hello\": \"value\"}".as_slice()),
                    [table, data] => (table, None, data.as_bytes()),
                    [table, id, data] => {
                        let eid = hex::decode(id).unwrap();
                        (table, Some(eid), data.as_bytes())
                    }
                    _ => {
                        println!("{{table}} [{{data}}]");
                        println!("{{table}} [{{id}}] [{{data}}]");
                        return None;
                    }
                };

                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let result = client
                    .send(models::tb_insert::Request {
                        id: *id,
                        op: models::tb_insert::Op {
                            scope: scope.clone(),
                            table: String::from(table),
                            eid,
                            value: data.to_vec(),
                        },
                    })
                    .await
                    .expect("unable to communicate");

                match result {
                    Ok(response) => {
                        let did = String::from_utf8(response.eid.clone()).unwrap_or_else(|err| {
                            let id = err.into_bytes();
                            hex::encode(id)
                        });

                        println!("Created '{:?}'", did);

                        Some(DbResponse::TbInsert(response))
                    }
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "ts_publish" => {
                let (measurement, fields, tags) = match *args {
                    [measurement] => (measurement, "", ""),
                    [measurement, fields] => (measurement, fields, ""),
                    [measurement, fields, tags] => (measurement, fields, tags),
                    _ => {
                        println!("{{measurement}} [{{tags}}] [{{fields}}]");
                        return None;
                    }
                };

                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let timestamp = session.new_timestamp().get_time().as_u64();

                let tags = if tags.is_empty() {
                    vec![]
                } else {
                    tags.split(',')
                        .map(|tag| {
                            let (key, value) = tag.split_once('=').unwrap_or((tag, ""));
                            (String::from(key), String::from(value))
                        })
                        .collect::<Vec<_>>()
                };

                let fields = if fields.is_empty() {
                    vec![]
                } else {
                    fields
                        .split(',')
                        .map(|tag| {
                            let (key, value) = tag.split_once('=').unwrap_or((tag, ""));
                            let value = match value.parse() {
                                Ok(value) => value,
                                Err(err) => match err {},
                            };

                            (String::from(key), value)
                        })
                        .collect::<Vec<_>>()
                };

                let req = models::ts_publish::Request {
                    id: *id,
                    op: models::ts_publish::Op {
                        scope: scope.clone(),
                        measurement: String::from(measurement),
                        tags,
                        fields,
                        timestamp,
                    },
                };

                let result = client.send(req).await.expect("unable to communicate");

                match result {
                    Ok(response) => Some(DbResponse::TsPublish(response)),
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "ts_find" => {
                let (measurement, order) = match *args {
                    [measurement] => (measurement, None),
                    [measurement, "asc"] => (measurement, Some(models::TsOrderBy::TimestampAsc)),
                    [measurement, "desc"] => (measurement, Some(models::TsOrderBy::TimestampDesc)),
                    _ => {
                        println!("{{measurement}} [asc|desc]");
                        return None;
                    }
                };

                let Some(id) = tx_id else {
                    println!("No open transaction");
                    return None;
                };

                let req = models::ts_find::Request {
                    id: *id,
                    op: models::ts_find::Op {
                        scope: scope.clone(),
                        measurement: String::from(measurement),
                        limit: None,
                        start: None,
                        end: None,
                        order,
                    },
                };

                let result = client.send(req).await.expect("unable to communicate");

                match result {
                    Ok(response) => {
                        let mut out = String::new();

                        for (tags, fields, timestamp) in &response.samples {
                            out.clear();

                            out.push_str(measurement);
                            out.push_str(" @ ");
                            write!(&mut out, "{}", timestamp).unwrap();

                            if !tags.is_empty() {
                                out.push_str(" (");

                                for (tag_key, tag_value) in tags {
                                    out.push_str(tag_key);
                                    out.push_str(" = ");
                                    out.push_str(tag_value);
                                    out.push(',');
                                }

                                out.push_str("):");
                            }

                            out.push_str(" {");
                            for (key, value) in fields {
                                out.push_str(key);
                                out.push_str(": ");
                                match value {
                                    FieldValue::I64(value) => {
                                        write!(&mut out, "{} as i64", value).unwrap();
                                    }
                                    FieldValue::U64(value) => {
                                        write!(&mut out, "{} as u64", value).unwrap();
                                    }
                                    FieldValue::F64(value) => {
                                        write!(&mut out, "{} as f64", value).unwrap();
                                    }
                                    FieldValue::String(value) => {
                                        write!(&mut out, "{} as String", value).unwrap();
                                    }
                                    FieldValue::Boolean(value) => {
                                        write!(&mut out, "{} as bool", value).unwrap();
                                    }
                                }
                                out.push(',');
                            }
                            out.push('}');

                            println!("{}", out);
                        }

                        Some(DbResponse::TsFind(response))
                    }
                    Err(err) => {
                        println!("{}", err.message);
                        None
                    }
                }
            }
            "" => None,
            _ => panic!("Unknown cmd: {}", cmd),
        }
    }
}
