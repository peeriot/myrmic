//! Drives the guest-facing db host functions through wasmtime, so a name check
//! dropped from one of them fails here and not only in the helpers' own tests.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::time::Duration;

use cell_protocol::{Gen, Sri};
use myrmic_common::db::{
    BlobHash, BlobId, BlobLinkRequest, BlobMoveRequest, BlobStoreRequest, BlobUnlinkRequest,
    Cursor, DeleteRequest, FindRequest, GetRequest, Measurement, PathResolveRequest,
    PathsListRequest, PrefixRequest, PublishRequest, PutRequest, Scope, TbAppendRequest,
    TbCountRequest, TbDeleteRequest, TbGetRequest, TbInsertRequest, TbListRequest, UpdateRequest,
};
use myrmic_common::types::error::{EINVAL, SUCCESS};
use sorg_common::SpawnLineage;
use wasmtime::{Engine, Extern, Instance, Linker, Module, Store, Val};

use crate::wasm::cell::state::CellState;

use super::super::link_db_functions;
use super::{MAX_KEY_PART_LEN, MAX_SEGMENT_LEN};

#[tokio::test(flavor = "multi_thread")]
async fn every_db_host_function_refuses_a_bad_name() {
    let mut guest = Guest::new().await;
    let long = "k".repeat(MAX_KEY_PART_LEN + 1);

    // The control: a name at the bound gets past every check, so the refusals
    // below are the checks' doing and not the harness's.
    let at_bound = PutRequest {
        scope: scope(),
        key: "k".repeat(MAX_KEY_PART_LEN),
        value: Vec::new(),
    };
    assert_eq!(guest.call("key_put", &at_bound).await, SUCCESS);

    let rejected = [
        // Key-value.
        ("key_put", enc(&put(scope(), &long))),
        (
            "key_delete",
            enc(&DeleteRequest {
                scope: scope(),
                key: long.clone(),
            }),
        ),
        ("key_get", enc(&get(scope(), &long))),
        (
            "key_prefix",
            enc(&PrefixRequest {
                scope: scope(),
                prefix: long.clone(),
            }),
        ),
        // Tables.
        ("tb_insert", enc(&insert(&long, None))),
        ("tb_insert", enc(&insert("t", Some(&long)))),
        ("tb_append", enc(&append(&long, None))),
        ("tb_append", enc(&append("t", Some(&long)))),
        ("tb_count", enc(&count(scope(), &long))),
        ("tb_get", enc(&tb_get(&long, &[]))),
        ("tb_get", enc(&tb_get("t", long.as_bytes()))),
        ("tb_list", enc(&list(&long, None))),
        (
            "tb_list",
            enc(&list("t", Some(Cursor::After(long.clone().into_bytes())))),
        ),
        (
            "tb_list",
            enc(&list("t", Some(Cursor::At(long.clone().into_bytes())))),
        ),
        ("tb_delete", enc(&tb_delete(&long, &[]))),
        ("tb_delete", enc(&tb_delete("t", long.as_bytes()))),
        // Time series.
        ("publish_measurement", enc(&publish(scope(), &long))),
        ("find_measurement", enc(&find(scope(), &long))),
        // Blobs.
        ("blob_link", enc(&link(scope(), &long))),
        ("blob_unlink", enc(&unlink(scope(), &long))),
        ("blob_move", enc(&mv(&long, "b"))),
        ("blob_move", enc(&mv("a", &long))),
        (
            "path_resolve",
            enc(&PathResolveRequest {
                scope: scope(),
                path: long.clone(),
                range: None,
            }),
        ),
    ];

    for (function, request) in rejected {
        assert_eq!(
            guest.call_raw(function, &request).await,
            EINVAL,
            "{function} accepted a bad name"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_db_host_function_refuses_a_bad_scope() {
    let mut guest = Guest::new().await;
    let long_segment = "s".repeat(MAX_SEGMENT_LEN + 1);

    // Too long, key-expression syntax, empty: per family, in each position.
    for bad in [long_segment.as_str(), "x?", "a/b", "*", ""] {
        for scope in [scope_in("d", bad), scope_in(bad, "s")] {
            let rejected = [
                ("key_put", enc(&put(scope.clone(), "k"))),
                ("key_get", enc(&get(scope.clone(), "k"))),
                ("tb_count", enc(&count(scope.clone(), "t"))),
                ("publish_measurement", enc(&publish(scope.clone(), "m"))),
                ("find_measurement", enc(&find(scope.clone(), "m"))),
                (
                    "blob_store",
                    enc(&BlobStoreRequest {
                        scope: scope.clone(),
                    }),
                ),
                ("blob_link", enc(&link(scope.clone(), "p"))),
                ("blob_unlink", enc(&unlink(scope.clone(), "p"))),
                (
                    "blob_move",
                    enc(&BlobMoveRequest {
                        scope: scope.clone(),
                        old_path: "a".into(),
                        new_path: "b".into(),
                    }),
                ),
                (
                    "paths_list",
                    enc(&PathsListRequest {
                        scope: scope.clone(),
                        limit: None,
                    }),
                ),
                (
                    "sem_update",
                    enc(&UpdateRequest {
                        scope,
                        query: String::new(),
                        base_iri: None,
                    }),
                ),
            ];

            for (function, request) in rejected {
                assert_eq!(
                    guest.call_raw(function, &request).await,
                    EINVAL,
                    "{function} accepted a bad scope {bad:?}"
                );
            }
        }
    }
}

struct Guest {
    store: Store<CellState>,
    instance: Instance,
}

impl Guest {
    /// A cell state over a loopback zenoh session, and a module exporting a
    /// wrapper for each db import: a host function reads the guest's memory
    /// through its caller, so it has to be called from inside the guest.
    async fn new() -> Self {
        let mut config = zenoh::Config::default();
        config
            .insert_json5("timestamping/enabled", "{ peer: true }")
            .expect("timestamping config");
        config
            .insert_json5("scouting/multicast/enabled", "false")
            .expect("scouting config");
        let session = zenoh::open(config).await.expect("unable to open session");

        let (state, _msg_rcv, _ready_rcv) = CellState::state_and_msg_rcv(
            Sri::of_path("wiring").expect("srn"),
            Gen::from_timestamp(&session.new_timestamp()),
            SpawnLineage::default(),
            session,
            HashSet::new(),
            Duration::from_secs(60),
            1,
        );

        let engine = Engine::default();
        let mut linker = Linker::new(&engine);
        link_db_functions(&mut linker).expect("unable to link the db functions");
        let mut store = Store::new(&engine, state);

        let functions: Vec<_> = linker
            .iter(&mut store)
            .filter_map(|(module, name, ext)| match ext {
                Extern::Func(func) => Some((module.to_owned(), name.to_owned(), func)),
                _ => None,
            })
            .collect();
        let mut imports = String::new();
        let mut wrappers = String::new();
        for (module, name, func) in functions {
            let params = func.ty(&store).params().len();
            let signature = " (param i32)".repeat(params);
            let forward = (0..params)
                .map(|i| format!("local.get {i}"))
                .collect::<Vec<_>>()
                .join(" ");

            writeln!(
                imports,
                "(import \"{module}\" \"{name}\" (func ${name}{signature} (result i32)))"
            )
            .expect("write");
            writeln!(
                wrappers,
                "(func (export \"{name}\"){signature} (result i32) {forward} call ${name})"
            )
            .expect("write");
        }
        let wat = format!("(module {imports} (memory (export \"memory\") 2) {wrappers})");
        let module = Module::new(&engine, wat).expect("wrapper module");
        let instance = linker
            .instantiate_async(&mut store, &module)
            .await
            .expect("unable to instantiate");

        Self { store, instance }
    }

    async fn call<T: serde::Serialize>(&mut self, function: &str, request: &T) -> i32 {
        self.call_raw(function, &enc(request)).await
    }

    /// Calls `function` with `request` at the start of the guest's memory and
    /// every further argument (response and blob buffers) pointing at its
    /// second page.
    async fn call_raw(&mut self, function: &str, request: &[u8]) -> i32 {
        const SECOND_PAGE: i32 = 65_536;

        let memory = self
            .instance
            .get_memory(&mut self.store, "memory")
            .expect("memory");
        memory.write(&mut self.store, 0, request).expect("write");

        let func = self
            .instance
            .get_func(&mut self.store, function)
            .unwrap_or_else(|| panic!("no db host function {function}"));
        let params = func.ty(&self.store).params().len();
        let mut args = vec![
            Val::I32(0),
            Val::I32(i32::try_from(request.len()).expect("len")),
        ];
        args.extend((2..params).map(|_| Val::I32(SECOND_PAGE)));
        let mut results = [Val::I32(0)];
        func.call_async(&mut self.store, &args, &mut results)
            .await
            .expect("host function trapped");

        results[0].unwrap_i32()
    }
}

fn enc<T: serde::Serialize>(request: &T) -> Vec<u8> {
    postcard::to_allocvec(request).expect("unable to encode")
}

fn scope() -> Scope {
    scope_in("d", "s")
}

fn scope_in(database: &str, schema: &str) -> Scope {
    Scope::public_owned("app".into(), Some(database.into()), Some(schema.into()))
}

fn put(scope: Scope, key: &str) -> PutRequest {
    PutRequest {
        scope,
        key: key.into(),
        value: Vec::new(),
    }
}

fn get(scope: Scope, key: &str) -> GetRequest {
    GetRequest {
        scope,
        key: key.into(),
    }
}

fn insert(table: &str, eid: Option<&str>) -> TbInsertRequest {
    TbInsertRequest {
        scope: scope(),
        table: table.into(),
        eid: eid.map(|eid| eid.as_bytes().to_vec()),
        value: Vec::new(),
    }
}

fn append(table: &str, eid: Option<&str>) -> TbAppendRequest {
    TbAppendRequest {
        scope: scope(),
        table: table.into(),
        eid: eid.map(|eid| eid.as_bytes().to_vec()),
        value: Vec::new(),
    }
}

fn count(scope: Scope, table: &str) -> TbCountRequest {
    TbCountRequest {
        scope,
        table: table.into(),
    }
}

fn tb_get(table: &str, eid: &[u8]) -> TbGetRequest {
    TbGetRequest {
        scope: scope(),
        table: table.into(),
        eid: eid.to_vec(),
    }
}

fn list(table: &str, cursor: Option<Cursor>) -> TbListRequest {
    TbListRequest {
        scope: scope(),
        table: table.into(),
        cursor,
        limit: None,
        order: None,
    }
}

fn tb_delete(table: &str, eid: &[u8]) -> TbDeleteRequest {
    TbDeleteRequest {
        scope: scope(),
        table: table.into(),
        eid: eid.to_vec(),
    }
}

fn publish(scope: Scope, name: &str) -> PublishRequest {
    PublishRequest {
        scope,
        measurement: Measurement {
            name: name.into(),
            tags: Vec::new(),
            fields: Vec::new(),
            ts: Some(1),
        },
    }
}

fn find(scope: Scope, name: &str) -> FindRequest {
    FindRequest {
        scope,
        measurement_name: name.into(),
        limit: None,
        start: None,
        end: None,
        order: None,
    }
}

fn link(scope: Scope, path: &str) -> BlobLinkRequest {
    BlobLinkRequest {
        blob_id: BlobId {
            scope,
            hash: BlobHash::Sha2([0; 32]),
        },
        path: path.into(),
    }
}

fn unlink(scope: Scope, path: &str) -> BlobUnlinkRequest {
    BlobUnlinkRequest {
        scope,
        path: path.into(),
    }
}

fn mv(old_path: &str, new_path: &str) -> BlobMoveRequest {
    BlobMoveRequest {
        scope: scope(),
        old_path: old_path.into(),
        new_path: new_path.into(),
    }
}
