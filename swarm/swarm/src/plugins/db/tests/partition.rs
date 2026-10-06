//! A network partition between two nodes, cut by a proxy that silently drops
//! every byte — no FIN, no RST — the way a pulled cable or a firewall rule
//! does.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

use super::{node_id, start_node_on, vouches_for};
use cell_protocol::node_lease_scope;
use db_client::v1::Client;
use db_commons::models;

/// Forwards loopback TCP to `upstream` until `cut` is set, then swallows
/// everything in both directions while keeping the sockets open.
async fn blackhole_proxy(upstream: u16, cut: Arc<AtomicBool>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    tokio::spawn(async move {
        loop {
            let Ok((inbound, _)) = listener.accept().await else {
                return;
            };
            let Ok(outbound) = TcpStream::connect(("127.0.0.1", upstream)).await else {
                continue;
            };
            let (in_r, in_w) = inbound.into_split();
            let (out_r, out_w) = outbound.into_split();
            tokio::spawn(pump(in_r, out_w, cut.clone()));
            tokio::spawn(pump(out_r, in_w, cut.clone()));
        }
    });

    port
}

async fn pump(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    cut: Arc<AtomicBool>,
) {
    let mut buf = vec![0; 64 * 1024];
    loop {
        let n = match from.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if cut.load(Ordering::Relaxed) {
            continue;
        }
        if to.write_all(&buf[..n]).await.is_err() {
            return;
        }
    }
}

async fn write_lease_row(client: &Client) -> Result<(), String> {
    client
        .write_tx_in(node_lease_scope(), async move |client, tx_id| {
            client
                .send(models::tb_insert::Request {
                    id: tx_id,
                    op: models::tb_insert::Op {
                        scope: node_lease_scope(),
                        table: cell_protocol::NODE_LEASE_TABLE.into(),
                        eid: Some(b"probe".to_vec()),
                        value: b"a".to_vec(),
                    },
                })
                .await?
                .map_err(|err| zenoh::Error::from(format!("insert refused: {}", err.message)))?;
            Ok(())
        })
        .await
        .map_err(|err| err.to_string())
}

/// Shortened from zenoh's 10 s default so the test detects the cut quickly.
const LINK_LEASE: Duration = Duration::from_secs(3);

/// A node-lease write fails on whichever side routes to the lost peer only
/// until zenoh's link lease notices the cut; after that, both sides of a
/// lasting partition write locally, every time.
#[tokio::test(flavor = "multi_thread")]
async fn writes_recover_once_the_link_lease_notices_a_partition() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let cut = Arc::new(AtomicBool::new(false));
    let proxy = blackhole_proxy(port, cut.clone()).await;

    let lease = LINK_LEASE.as_millis().to_string();
    let listen = format!(r#"["tcp/127.0.0.1:{port}"]"#);
    let connect = format!(r#"["tcp/127.0.0.1:{proxy}"]"#);
    let (a, _drop_a) = start_node_on(
        |z| {
            z.insert_json5("listen/endpoints", &listen).unwrap();
            z.insert_json5("transport/link/tx/lease", &lease).unwrap();
        },
        Default::default(),
    )
    .await;
    let (b, _drop_b) = start_node_on(
        |z| {
            z.insert_json5("connect/endpoints", &connect).unwrap();
            z.insert_json5("transport/link/tx/lease", &lease).unwrap();
        },
        Default::default(),
    )
    .await;
    let scope = node_lease_scope();
    let client_a = Client::new(&a);
    let client_b = Client::new(&b);

    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        write_lease_row(&client_a).await.expect("a's write failed");
        write_lease_row(&client_b).await.expect("b's write failed");
        if vouches_for(&a, &scope, node_id(&b)).await && vouches_for(&b, &scope, node_id(&a)).await
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the pair never vouched for each other"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    cut.store(true, Ordering::Relaxed);

    let deadline = Instant::now() + LINK_LEASE + Duration::from_secs(5);
    while vouches_for(&a, &scope, node_id(&b)).await || vouches_for(&b, &scope, node_id(&a)).await {
        assert!(
            Instant::now() < deadline,
            "a partitioned peer was still vouched for past the link lease"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let until = Instant::now() + Duration::from_secs(10);
    while Instant::now() < until {
        write_lease_row(&client_a)
            .await
            .expect("a's write failed during the partition");
        write_lease_row(&client_b)
            .await
            .expect("b's write failed during the partition");
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}
