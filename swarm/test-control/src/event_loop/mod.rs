mod events;

use std::future::Future;
use std::str::FromStr;
use std::time::Duration;
use std::{collections::HashMap, sync::Arc};

use test_control_common::{
    Client, Error, IntrospectionClient, PoisonRcv, Reply, Request, SorgPayload, bail, zenoh_err,
};
use tokio::{
    select,
    sync::{
        Mutex,
        mpsc::{Receiver, channel},
    },
    task::JoinHandle,
    time::sleep,
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing::{debug, error};
use zenoh::{Session, config::ZenohId, query::Query};

use crate::Result;

pub(crate) use events::Event;

type EventReceiver = Receiver<Event>;
type KeyExpr = String;
type Id = String;

const EVENT_BUFFER_SIZE: usize = 10;

pub(crate) fn set_up_event_loop(
    session: Session,
    poison_rcv: PoisonRcv,
) -> (Client<Event>, JoinHandle<Result<()>>) {
    let (event_sender, event_receiver) = channel(EVENT_BUFFER_SIZE);
    let client = Client::new(event_sender);
    let join_handle = tokio::spawn(event_loop(session, event_receiver, poison_rcv));
    (client, join_handle)
}

async fn event_loop(
    session: Session,
    mut event_rcv: EventReceiver,
    mut poison_rcv: PoisonRcv,
) -> Result<()> {
    let mut controller = Controller::new(session);

    loop {
        select! {
            event = event_rcv.recv() => {
                let Some(event) = event else {
                    error!("event channel closed");
                    break;
                };
                controller.process_event(event).await?;
            }

            _ = &mut poison_rcv => {
                debug!("shutting down test control event loop");
                break;
            }
        }
    }

    controller.close_and_wait().await;

    Ok(())
}

struct Controller {
    session: Session,

    counters: Arc<Mutex<Counters>>,

    task_tracker: TaskTracker,
    cancellation_tokens: Arc<Mutex<HashMap<Id, CancellationToken>>>,
}

impl Controller {
    fn new(session: Session) -> Self {
        Self {
            session,
            counters: Arc::new(Mutex::new(Counters::default())),
            task_tracker: TaskTracker::new(),
            cancellation_tokens: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn process_event(&mut self, event: Event) -> Result<()> {
        match event {
            Event::CreatePublisherQuery(query) => self.create_publisher(query).await,
            Event::DeletePublisherQuery(query) => self.delete_publisher(query).await,
            Event::CreateSubscriberQuery(query) => self.create_subscriber(query).await,
            Event::DeleteSubscriberQuery(query) => self.delete_subscriber(query).await,
            Event::CreateQueryableQuery(query) => self.create_queryable(query).await,
            Event::DeleteQueryableQuery(query) => self.delete_queryable(query).await,
            Event::PutQuery(query) => self.put(query).await,
            Event::GetQuery(query) => self.get(query).await,
            Event::DeleteQuery(query) => self.delete(query).await,
            Event::StatsQuery(query) => self.stats(query).await,
            Event::Health(query) => self.health(query).await,
            Event::Introspection(query) => self.introspection(query).await,
        }
    }

    async fn create_publisher(&mut self, query: Query) -> Result<()> {
        debug!("handle create publisher");

        let Some(payload) = query.payload() else {
            bail!("creating publisher without payload")
        };

        let Request::CreatePublisher {
            zid,
            key_expr,
            payload,
            count,
            delay,
        } = Request::from_payload(payload, "test: deserializing CreatePublisher request")?
        else {
            bail!("wrong payload for creating publisher")
        };

        if !self.zid_matches_session(&zid).await? {
            return Ok(());
        }

        let pub_id = gen_id_with_prefix("pub");
        let z_pub = self
            .session
            .declare_publisher(key_expr.clone())
            .await
            .map_err(|zen_err| {
                zenoh_err!("create publisher - failed to declare publisher:", zen_err)
            })?;

        let mut remaining = count;

        let key_clone = key_expr.clone();
        let counters = self.counters.clone();

        self.spawn_with_cancellation(pub_id.clone(), move |token| async move {
            loop {
                select! {
                    () = token.cancelled() => {
                        let _ = z_pub.undeclare().await;
                        break;
                    }
                    () = async {
                        let _ = z_pub.put(payload.clone()).await;
                        counters.lock().await.inc_sent(&key_clone);

                        if let Some(d) = delay { sleep(d).await; }
                    } => {
                        if let Some(n) = remaining.as_mut() {
                            *n = n.checked_sub(1).unwrap_or(0);
                            if *n == 0 {
                                let _ = z_pub.undeclare().await;
                                break;
                            }
                        }
                    }
                }
            }
        })
        .await;

        let reply_payload = Reply::PublisherCreated {
            ok: true,
            pub_id,
            key_expr,
        }
        .to_payload()?;
        query
            .reply(query.key_expr(), reply_payload)
            .await
            .map_err(|zen_err| {
                zenoh_err!("create publisher - failed to reply the query", zen_err)
            })?;

        Ok(())
    }

    async fn delete_publisher(&mut self, query: Query) -> Result<()> {
        debug!("handle delete publisher");

        let Some(payload) = query.payload() else {
            bail!("deleting publisher without payload")
        };

        let Request::DeletePublisher { zid, pub_id } =
            Request::from_payload(payload, "test: deserializing DeletePublisher request")?
        else {
            bail!("wrong payload for deleting publisher")
        };

        if !self.zid_matches_session(&zid).await? {
            return Ok(());
        }

        let ok = self.cancel_by_id(&pub_id);

        let reply_payload = Reply::PublisherDeleted { ok, pub_id }.to_payload()?;
        query
            .reply(query.key_expr(), reply_payload)
            .await
            .map_err(|zen_err| {
                zenoh_err!("delete publisher - failed to reply the query", zen_err)
            })?;

        Ok(())
    }

    async fn create_subscriber(&mut self, query: Query) -> Result<()> {
        debug!("handle create subscriber");

        let Some(payload) = query.payload() else {
            bail!("creating subscriber without payload")
        };

        let Request::CreateSubscriber {
            zid,
            key_expr,
            max_samples,
            stream_key,
        } = Request::from_payload(payload, "test: deserializing CreateSubscriber request")?
        else {
            bail!("wrong payload for creating subscriber")
        };

        if !self.zid_matches_session(&zid).await? {
            return Ok(());
        }

        let sub_id = gen_id_with_prefix("sub");
        let sub = self
            .session
            .declare_subscriber(&key_expr)
            .await
            .map_err(|zen_err| {
                zenoh_err!("create subscriber - failed to declare subscriber:", zen_err)
            })?;

        let session = self.session.clone();
        let counters = self.counters.clone();
        let key_expr_clone = key_expr.clone();
        let mut left = max_samples;

        self.spawn_with_cancellation(sub_id.clone(), move |token| async move {
            loop {
                select! {
                    () = token.cancelled() => {
                        let _ = sub.undeclare().await;
                        break;
                    }

                    recv = sub.recv_async() => {
                        match recv {
                            Ok(sample) => {
                                counters.lock().await.inc_recv(&key_expr_clone);

                                if let Some(sk) = &stream_key {
                                    let _ = session.put(sk, sample.payload().clone()).await;
                                }

                                if let Some(n) = left.as_mut() {
                                    *n = n.checked_sub(1).unwrap_or(0);
                                    if *n == 0 {
                                        let _ = sub.undeclare().await;
                                        break;
                                    }
                                }
                            }
                            Err(_) => {
                                break;
                            }
                        }
                    }
                }
            }
        })
        .await;

        let reply_payload = Reply::SubscriberCreated {
            ok: true,
            key_expr,
            sub_id,
        }
        .to_payload()?;
        query
            .reply(query.key_expr(), reply_payload)
            .await
            .map_err(|zen_err| {
                zenoh_err!("create subscriber - failed to reply the query", zen_err)
            })?;

        Ok(())
    }

    async fn delete_subscriber(&mut self, query: Query) -> Result<()> {
        debug!("handle delete subscriber");

        let Some(payload) = query.payload() else {
            bail!("deleting subscriber without payload")
        };

        let Request::DeleteSubscriber { zid, sub_id } =
            Request::from_payload(payload, "test: deserializing DeleteSubscriber request")?
        else {
            bail!("wrong payload for deleting subscriber")
        };

        if !self.zid_matches_session(&zid).await? {
            return Ok(());
        }

        let ok = self.cancel_by_id(&sub_id);

        let reply_payload = Reply::SubscriberDeleted { sub_id, ok }.to_payload()?;
        query
            .reply(query.key_expr(), reply_payload)
            .await
            .map_err(|zen_err| {
                zenoh_err!("delete subscriber - failed to reply the query", zen_err)
            })?;

        Ok(())
    }

    async fn create_queryable(&mut self, query: Query) -> Result<()> {
        debug!("handle create queryable");

        let Some(payload) = query.payload() else {
            bail!("creating queryable without payload")
        };

        let Request::CreateQueryable {
            zid,
            key_expr,
            static_payload,
        } = Request::from_payload(payload, "test: deserializing CreateQueryable request")?
        else {
            bail!("wrong payload for creating queryable")
        };

        if !self.zid_matches_session(&zid).await? {
            return Ok(());
        }

        let qbl_id = gen_id_with_prefix("qbl");
        let qbl = self
            .session
            .declare_queryable(&key_expr)
            .await
            .map_err(|zen_err| {
                zenoh_err!("create queryable - failed to declare queryable:", zen_err)
            })?;

        let counters = self.counters.clone();
        let key_expr_clone = key_expr.clone();

        self.spawn_with_cancellation(qbl_id.clone(), move |token| async move {
            loop {
                select! {
                    () = token.cancelled() => {
                        let _ = qbl.undeclare().await;
                        break;
                    }

                    recv = qbl.recv_async() => {
                        match recv {
                            Ok(q) => {
                                let _ = q.reply(key_expr_clone.as_str(), static_payload.clone()).await;
                                counters.lock().await.inc_qbl(&key_expr_clone);
                            }
                            Err(_) => {
                                break;
                            }
                        }
                    }
                }
            }
        }).await;

        let reply_payload = Reply::QueryableCreated {
            ok: true,
            qbl_id,
            key_expr,
        }
        .to_payload()?;
        query
            .reply(query.key_expr(), reply_payload)
            .await
            .map_err(|zen_err| {
                zenoh_err!("create queryable - failed to reply the query", zen_err)
            })?;

        Ok(())
    }

    async fn delete_queryable(&mut self, query: Query) -> Result<()> {
        debug!("handle delete queryable");

        let Some(payload) = query.payload() else {
            bail!("deleting queryable without payload")
        };

        let Request::DeleteQueryable { zid, qbl_id } =
            Request::from_payload(payload, "test: deserializing DeleteQueryable request")?
        else {
            bail!("wrong payload for deleting queryable")
        };

        if !self.zid_matches_session(&zid).await? {
            return Ok(());
        }

        let ok = self.cancel_by_id(&qbl_id);

        let reply_payload = Reply::QueryableDeleted { ok, qbl_id }.to_payload()?;
        query
            .reply(query.key_expr(), reply_payload)
            .await
            .map_err(|zen_err| {
                zenoh_err!("delete queryable - failed to reply the query", zen_err)
            })?;

        Ok(())
    }

    async fn put(&mut self, query: Query) -> Result<()> {
        debug!("handle put");

        let Some(payload) = query.payload() else {
            bail!("putting data without payload")
        };

        let Request::Put {
            zid,
            key_expr,
            payload: val,
        } = Request::from_payload(payload, "test: deserializing Put request")?
        else {
            bail!("wrong payload for putting data")
        };

        if !self.zid_matches_session(&zid).await? {
            return Ok(());
        }

        self.session
            .put(&key_expr, val.clone())
            .await
            .map_err(|zen_err| {
                zenoh_err!("put data - failed to put data to the session:", zen_err)
            })?;

        self.counters.lock().await.inc_sent(&key_expr);

        let reply_payload = Reply::Put { ok: true, key_expr }.to_payload()?;
        query
            .reply(query.key_expr(), reply_payload)
            .await
            .map_err(|zen_err| zenoh_err!("put data - failed to reply the query", zen_err))?;

        Ok(())
    }

    async fn get(&mut self, query: Query) -> Result<()> {
        debug!("handle get");

        let Some(payload) = query.payload() else {
            bail!("getting data without payload")
        };

        let Request::Get {
            zid,
            key_expr,
            timeout_ms,
        } = Request::from_payload(payload, "test: deserializing Get request")?
        else {
            bail!("wrong payload for getting data")
        };

        if !self.zid_matches_session(&zid).await? {
            return Ok(());
        }

        let get_id = gen_id_with_prefix("get");
        let timeout = Duration::from_secs(timeout_ms.unwrap_or(10));

        let replies = self
            .session
            .get(&key_expr)
            .timeout(timeout)
            .await
            .map_err(|zen_err| {
                zenoh_err!("get data - failed to get data from the session:", zen_err)
            })?;

        let counters = self.counters.clone();
        let key_expr_clone = key_expr.clone();

        self.spawn_tracked(async move {
            while let Ok(rep) = replies.recv_async().await {
                if let Ok(_sample) = rep.result() {
                    counters.lock().await.inc_get(&key_expr_clone);
                }
            }
        });

        let reply_payload = Reply::Get {
            ok: true,
            key_expr,
            get_id,
        }
        .to_payload()?;
        query
            .reply(query.key_expr(), reply_payload)
            .await
            .map_err(|zen_err| zenoh_err!("get data - failed to reply the query", zen_err))?;

        Ok(())
    }

    async fn delete(&mut self, query: Query) -> Result<()> {
        debug!("handle delete");

        let Some(payload) = query.payload() else {
            bail!("deleting data without payload")
        };

        let Request::Delete { zid, key_expr } =
            Request::from_payload(payload, "test: deserializing Delete request")?
        else {
            bail!("wrong payload for deleting data")
        };

        if !self.zid_matches_session(&zid).await? {
            return Ok(());
        }

        self.session.delete(&key_expr).await.map_err(|zen_err| {
            zenoh_err!("delete data - failed to put data to the session:", zen_err)
        })?;

        let reply_payload = Reply::Delete { ok: true, key_expr }.to_payload()?;
        query
            .reply(query.key_expr(), reply_payload)
            .await
            .map_err(|zen_err| zenoh_err!("delete data - failed to reply the query", zen_err))?;

        Ok(())
    }

    async fn stats(&self, query: Query) -> Result<()> {
        debug!("handle stats");

        let Some(payload) = query.payload() else {
            bail!("getting stats without payload")
        };

        let Request::Stats { zid, key_expr } =
            Request::from_payload(payload, "test: deserializing Stats request")?
        else {
            bail!("wrong payload for getting stats")
        };

        if !self.zid_matches_session(&zid).await? {
            return Ok(());
        }

        let (sent, received, gets, queries) = {
            let c = self.counters.lock().await;
            (
                c.get_sent(&key_expr),
                c.get_recv(&key_expr),
                c.get_get(&key_expr),
                c.get_qbl(&key_expr),
            )
        };

        let reply = Reply::Stats {
            ok: true,
            key_expr,
            sent,
            received,
            gets,
            queries,
        }
        .to_payload()?;
        query
            .reply(query.key_expr(), reply)
            .await
            .map_err(|zen_err| zenoh_err!("get stats - failed to reply the query", zen_err))?;

        Ok(())
    }

    async fn health(&self, query: Query) -> Result<()> {
        debug!("handle health");

        let Some(payload) = query.payload() else {
            bail!("getting health without payload")
        };

        let Request::Health { zid } =
            Request::from_payload(payload, "test: deserializing Health request")?
        else {
            bail!("wrong payload for getting health")
        };

        if !self.zid_matches_session(&zid).await? {
            return Ok(());
        }

        let reply = Reply::Health { ok: true }.to_payload()?;
        query
            .reply(query.key_expr(), reply)
            .await
            .map_err(|zen_err| zenoh_err!("get health - failed to reply the query", zen_err))?;

        Ok(())
    }

    async fn introspection(&self, query: Query) -> Result<()> {
        debug!("handle introspection");

        let Some(payload) = query.payload() else {
            bail!("requesting introspection without payload")
        };

        let Request::Introspection { zid } =
            Request::from_payload(payload, "test: deserializing Introspection request")?
        else {
            bail!("wrong payload for requesting introspection")
        };

        if !self.zid_matches_session(&zid).await? {
            return Ok(());
        }

        let client = IntrospectionClient::new(self.session.clone()).await;
        let nodes_status = client
            .swarm_status()
            .await
            .map_err(|zen_err| Error::Custom(zen_err.to_string()))?;

        let reply = Reply::Introspection { nodes_status }.to_payload()?;
        query
            .reply(query.key_expr(), reply)
            .await
            .map_err(|zen_err| zenoh_err!("introspection - failed to reply the query", zen_err))?;

        Ok(())
    }

    /// Spawn a tracked task with cancellation by id via `CancellationToken`.
    async fn spawn_with_cancellation<Fut, MakeFut>(&self, id: Id, make_future: MakeFut)
    where
        Fut: Future<Output = ()> + Send + 'static,
        MakeFut: FnOnce(CancellationToken) -> Fut + Send + 'static,
    {
        let token = CancellationToken::new();

        {
            let mut map = self.cancellation_tokens.lock().await;
            map.insert(id.clone(), token.clone());
        }

        let child = token.child_token();
        let tokens = Arc::clone(&self.cancellation_tokens);

        self.task_tracker.spawn(async move {
            make_future(child).await;

            // self-clean on exit
            let mut map = tokens.lock().await;
            let _ = map.remove(&id);
        });
    }

    /// Spawn a task without cancellation.
    fn spawn_tracked<Fut>(&self, fut: Fut)
    where
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.task_tracker.spawn(fut);
    }

    /// Try to cancel a running task by `id`.
    fn cancel_by_id(&self, id: &Id) -> bool {
        if let Some(token) = self.cancellation_tokens.blocking_lock().get(id).cloned() {
            token.cancel();
            true
        } else {
            false
        }
    }

    async fn close_and_wait(&self) {
        self.task_tracker.close();
        {
            let map = self.cancellation_tokens.lock().await;
            for token in map.values() {
                token.cancel();
            }
        }
        self.task_tracker.wait().await;
    }

    async fn zid_matches_session(&self, zid_str: &str) -> Result<bool> {
        let Ok(zid) = ZenohId::from_str(zid_str) else {
            bail!("failed to parse zid for request");
        };

        Ok(self.session.info().zid().await == zid)
    }
}

#[derive(Default)]
struct Counters {
    sent: HashMap<KeyExpr, u32>,
    recv: HashMap<KeyExpr, u32>,
    qbl: HashMap<KeyExpr, u32>,
    get: HashMap<KeyExpr, u32>,
}

impl Counters {
    #[inline]
    fn inc_sent(&mut self, id: &str) {
        *self.sent.entry(id.into()).or_insert(0) += 1;
    }
    #[inline]
    fn inc_recv(&mut self, id: &str) {
        *self.recv.entry(id.into()).or_insert(0) += 1;
    }
    #[inline]
    fn inc_qbl(&mut self, id: &str) {
        *self.qbl.entry(id.into()).or_insert(0) += 1;
    }
    #[inline]
    fn inc_get(&mut self, id: &str) {
        *self.get.entry(id.into()).or_insert(0) += 1;
    }

    #[inline]
    fn get_sent(&self, id: &str) -> u32 {
        self.sent.get(id).copied().unwrap_or(0)
    }
    #[inline]
    fn get_recv(&self, id: &str) -> u32 {
        self.recv.get(id).copied().unwrap_or(0)
    }
    #[inline]
    fn get_qbl(&self, id: &str) -> u32 {
        self.qbl.get(id).copied().unwrap_or(0)
    }
    #[inline]
    fn get_get(&self, id: &str) -> u32 {
        self.get.get(id).copied().unwrap_or(0)
    }
}

fn gen_id_with_prefix(prefix: &str) -> String {
    format!("{}-{}", prefix, uuid::Uuid::new_v4())
}
