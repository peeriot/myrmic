use crate::{Context, DbResponse};

const TEST_CONFIG: &str = r#"
local z = import "zenoh.libsonnet";

z.peer()
+ z.plugins.dev({ db: {} })
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test() {
    let mut ctx = Context::with_config(TEST_CONFIG).await;

    ctx.execute_line("begin").await;
    ctx.execute_line("ts_publish cpu_temp value=123.3").await;
    ctx.execute_line("ts_publish cpu_temp value=123.4").await;
    ctx.execute_line("ts_publish cpu_temp value=123.5").await;
    ctx.execute_line("ts_publish cpu_temp value=123.6").await;
    ctx.execute_line("ts_publish cpu_temp value=123.7").await;
    ctx.execute_line("ts_publish cpu_temp value=123.8").await;
    ctx.execute_line("ts_publish cpu_temp value=123.9").await;
    ctx.execute_line("commit").await;

    ctx.execute_line("begin").await;
    let Some(DbResponse::TsFind(mut response)) = ctx.execute_line("ts_find cpu_temp").await else {
        panic!("unexpected response");
    };

    assert_eq!(response.samples.len(), 7);

    response.samples.sort_by_key(|left| left.2);

    let fields = response
        .samples
        .into_iter()
        .map(|item| item.1)
        .filter_map(|mut item| item.pop())
        .map(|(_key, value)| value)
        .collect::<Vec<_>>();

    assert_eq!(
        fields,
        vec![
            "123.3".parse().unwrap(),
            "123.4".parse().unwrap(),
            "123.5".parse().unwrap(),
            "123.6".parse().unwrap(),
            "123.7".parse().unwrap(),
            "123.8".parse().unwrap(),
            "123.9".parse().unwrap(),
        ]
    );

    ctx.execute_line("rollback").await;

    ctx.execute_line("begin").await;
    let Some(DbResponse::BlobStore(response)) = ctx.execute_line("blob_store hello_blob").await
    else {
        panic!("unexpected response");
    };

    let hash = response.blob_id.hash.to_hex();

    let Some(DbResponse::BlobLink(_)) = ctx
        .execute_line(&format!("blob_link {hash} /hello.txt"))
        .await
    else {
        panic!("unexpected response");
    };

    let Some(DbResponse::PathResolve(response)) = ctx.execute_line("path_resolve /hello.txt").await
    else {
        panic!("unexpected response");
    };
    assert_eq!(
        response.blob.as_ref().map(|r| r.blob.as_slice()),
        Some(b"hello_blob".as_ref())
    );

    let Some(DbResponse::BlobResolve(response)) =
        ctx.execute_line(&format!("blob_resolve {hash}")).await
    else {
        panic!("unexpected response");
    };
    assert_eq!(
        response.blob.as_ref().map(|r| r.blob.as_slice()),
        Some(b"hello_blob".as_ref())
    );

    let Some(DbResponse::BlobMove(_)) = ctx
        .execute_line("blob_move /hello.txt /hello-moved.txt")
        .await
    else {
        panic!("unexpected response");
    };

    let Some(DbResponse::PathResolve(response)) = ctx.execute_line("path_resolve /hello.txt").await
    else {
        panic!("unexpected response");
    };
    assert!(response.blob.is_none());

    let Some(DbResponse::PathResolve(response)) =
        ctx.execute_line("path_resolve /hello-moved.txt").await
    else {
        panic!("unexpected response");
    };
    assert_eq!(
        response.blob.as_ref().map(|r| r.blob.as_slice()),
        Some(b"hello_blob".as_ref())
    );

    let Some(DbResponse::BlobUnlink(_)) = ctx.execute_line("blob_unlink /hello-moved.txt").await
    else {
        panic!("unexpected response");
    };

    let Some(DbResponse::PathResolve(response)) =
        ctx.execute_line("path_resolve /hello-moved.txt").await
    else {
        panic!("unexpected response");
    };
    assert!(response.blob.is_none());
}
