//! Complete, bounded, runtime/thread-bound MCP catalog snapshots.
//! Successful acquisition is evidence of definitions, never approval of tools.
use crate::runtime::CodexRuntime;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Semaphore, watch},
    time::Instant,
};

const PAGE_BYTES: usize = 8 * 1024 * 1024;
const TOTAL_BYTES: usize = 32 * 1024 * 1024;
const KEY_RESERVATION: usize = 4096;
const KEYS: usize = 32;

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Key {
    pub instance: String,
    pub recovery: String,
    pub runtime: String,
    pub thread: String,
    pub config: String,
}
impl Key {
    fn valid(&self) -> bool {
        [
            &self.instance,
            &self.recovery,
            &self.runtime,
            &self.thread,
            &self.config,
        ]
        .iter()
        .all(|s| !s.is_empty() && s.len() <= 128)
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Server {
    pub runtime_status: String,
    pub auth_status: String,
    pub tools: BTreeMap<String, String>,
}
impl Server {
    pub fn available(&self) -> Result<(), &'static str> {
        if self.runtime_status == "authenticationRequired" || self.auth_status == "notLoggedIn" {
            return Err("catalog_auth_required");
        }
        if self.runtime_status != "connected" {
            return Err("catalog_server_unavailable");
        }
        if self.auth_status == "unknown" {
            return Err("catalog_server_unavailable");
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub guard_target: Option<super::approval_config::Target>,
    pub servers: BTreeMap<String, Server>,
}
impl Snapshot {
    fn reserved_bytes(&self) -> usize {
        KEY_RESERVATION
            + if self.guard_target.is_some() { 1024 } else { 0 }
            + self
                .servers
                .iter()
                .map(|(name, s)| {
                    // Include map nodes, struct/Arc allocations and allocator overhead.
                    512 + name.capacity()
                        + s.runtime_status.capacity()
                        + s.auth_status.capacity()
                        + s.tools
                            .iter()
                            .map(|(n, h)| 512 + n.capacity() + h.capacity())
                            .sum::<usize>()
                })
                .sum::<usize>()
    }
}
#[derive(Clone, Debug)]
pub enum Status {
    Loading,
    Ready {
        snapshot: Arc<Snapshot>,
        epoch: String,
    },
    Failed(&'static str),
}
struct Entry {
    ticket: String,
    status: watch::Sender<Status>,
    completed: Option<Instant>,
    previous: Option<(Arc<Snapshot>, String)>,
    reserved: usize,
    worker: Option<tokio::task::AbortHandle>,
}
#[derive(Default)]
struct State {
    entries: BTreeMap<Key, Entry>,
    runtimes: BTreeMap<String, Arc<Semaphore>>,
}
#[derive(Clone, Default)]
pub struct Manager {
    state: Arc<Mutex<State>>,
}
impl Manager {
    /// Does not wait for upstream. All callers of one key observe one worker.
    pub fn request(
        &self,
        key: Key,
        runtime: Arc<CodexRuntime>,
    ) -> Result<watch::Receiver<Status>, &'static str> {
        if key.runtime != runtime.id() {
            return Err("catalog_binding_mismatch");
        }
        let obsolete = self
            .state
            .lock()
            .unwrap()
            .entries
            .keys()
            .filter(|existing| {
                existing.instance == key.instance
                    && existing.recovery == key.recovery
                    && existing.runtime == key.runtime
                    && existing.thread == key.thread
                    && existing.config != key.config
            })
            .cloned()
            .collect::<Vec<_>>();
        for previous in obsolete {
            self.invalidate(&previous);
        }
        self.start(key, move |key| async move {
            collect(&runtime, &key.thread).await
        })
    }
    fn start<F, Fut>(&self, key: Key, fetch: F) -> Result<watch::Receiver<Status>, &'static str>
    where
        F: FnOnce(Key) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Snapshot, &'static str>> + Send + 'static,
    {
        if !key.valid() {
            return Err("catalog_invalid");
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut state = self.state.lock().unwrap();
        if let Some(entry) = state.entries.get(&key) {
            let status = entry.status.borrow();
            let ttl = if matches!(*status, Status::Ready { .. }) {
                30
            } else {
                2
            };
            if entry.completed.is_none()
                || entry
                    .completed
                    .is_some_and(|t| t.elapsed() < Duration::from_secs(ttl))
            {
                return Ok(entry.status.subscribe());
            }
        } else if state.entries.len() >= KEYS {
            return Err("catalog_capacity");
        }
        if !state.entries.contains_key(&key)
            && state.entries.values().map(|e| e.reserved).sum::<usize>() + KEY_RESERVATION
                > TOTAL_BYTES
        {
            return Err("catalog_capacity");
        }
        let live: BTreeSet<_> = state.entries.keys().map(|k| k.runtime.clone()).collect();
        state
            .runtimes
            .retain(|id, slots| live.contains(id) || Arc::strong_count(slots) > 1);
        let slots = state
            .runtimes
            .entry(key.runtime.clone())
            .or_insert_with(|| Arc::new(Semaphore::new(2)))
            .clone();
        let ticket = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = if let Some(entry) = state.entries.get(&key) {
            (entry.status.clone(), entry.status.subscribe())
        } else {
            watch::channel(Status::Loading)
        };
        let previous = state
            .entries
            .get(&key)
            .and_then(|e| match &*e.status.borrow() {
                Status::Ready { snapshot, epoch } => Some((snapshot.clone(), epoch.clone())),
                _ => None,
            });
        let reserved = previous
            .as_ref()
            .map(|(s, _)| s.reserved_bytes())
            .unwrap_or(KEY_RESERVATION);
        tx.send_replace(Status::Loading);
        state.entries.insert(
            key.clone(),
            Entry {
                ticket: ticket.clone(),
                status: tx,
                completed: None,
                previous,
                reserved,
                worker: None,
            },
        );
        drop(state);
        let manager = self.clone();
        let cleanup = WorkerCleanup {
            manager: self.clone(),
            key: key.clone(),
            ticket: ticket.clone(),
        };
        let worker_key = key.clone();
        let worker_ticket = ticket.clone();
        let worker = tokio::spawn(async move {
            let _cleanup = cleanup;
            let result = tokio::time::timeout_at(deadline, async {
                let _permit = slots.acquire().await.map_err(|_| "catalog_unavailable")?;
                fetch(key.clone()).await
            })
            .await
            .unwrap_or(Err("catalog_timeout"));
            let mut state = manager.state.lock().unwrap();
            if state.entries.get(&key).is_none_or(|e| e.ticket != ticket) {
                return;
            }
            let others: usize = state
                .entries
                .iter()
                .filter(|(k, _)| **k != key)
                .map(|(_, e)| e.reserved)
                .sum();
            let entry = state.entries.get_mut(&key).unwrap();
            let status = match result {
                Ok(snapshot) if snapshot.reserved_bytes() <= TOTAL_BYTES.saturating_sub(others) => {
                    entry.reserved = snapshot.reserved_bytes();
                    let epoch = entry
                        .previous
                        .as_ref()
                        .filter(|(old, _)| **old == snapshot)
                        .map(|(_, epoch)| epoch.clone())
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    Status::Ready {
                        snapshot: Arc::new(snapshot),
                        epoch,
                    }
                }
                Ok(_) => Status::Failed("catalog_capacity"),
                Err(code) => Status::Failed(code),
            };
            if matches!(status, Status::Failed(_)) {
                entry.reserved = KEY_RESERVATION;
            }
            entry.previous = None;
            entry.completed = Some(Instant::now());
            entry.status.send_replace(status);
        });
        let mut state = self.state.lock().unwrap();
        if let Some(entry) = state
            .entries
            .get_mut(&worker_key)
            .filter(|e| e.ticket == worker_ticket)
        {
            entry.worker = Some(worker.abort_handle());
        } else {
            worker.abort();
        }
        Ok(rx)
    }
    /// In-flight results cannot publish after invalidation. Existing subscribers
    /// get an explicit error; next acquisition starts a fresh epoch.
    pub fn invalidate(&self, key: &Key) {
        let mut state = self.state.lock().unwrap();
        if let Some(mut entry) = state.entries.remove(key) {
            entry
                .status
                .send_replace(Status::Failed("catalog_invalidated"));
            entry.previous = None;
            if let Some(worker) = entry.worker {
                worker.abort();
            }
        }
        if !state.entries.keys().any(|k| k.runtime == key.runtime)
            && state
                .runtimes
                .get(&key.runtime)
                .is_some_and(|s| Arc::strong_count(s) == 1)
        {
            state.runtimes.remove(&key.runtime);
        }
    }
    pub fn peek(&self, key: &Key) -> Status {
        let state = self.state.lock().unwrap();
        let Some(entry) = state.entries.get(key) else {
            return Status::Loading;
        };
        let status = entry.status.borrow().clone();
        if matches!(status, Status::Ready { .. })
            && entry
                .completed
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(30))
        {
            Status::Loading
        } else {
            status
        }
    }
    pub fn release(&self, key: &Key) {
        self.invalidate(key);
    }
    pub fn release_thread(&self, runtime: &str, thread: &str) {
        let keys = self
            .state
            .lock()
            .unwrap()
            .entries
            .keys()
            .filter(|key| key.runtime == runtime && key.thread == thread)
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            self.invalidate(&key);
        }
    }
    pub async fn view(mut receiver: watch::Receiver<Status>) -> Status {
        let status = receiver.borrow_and_update().clone();
        if !matches!(status, Status::Loading) {
            return status;
        }
        let _ = tokio::time::timeout(Duration::from_millis(250), receiver.changed()).await;
        receiver.borrow().clone()
    }
}

struct WorkerCleanup {
    manager: Manager,
    key: Key,
    ticket: String,
}
impl Drop for WorkerCleanup {
    fn drop(&mut self) {
        let mut state = self.manager.state.lock().unwrap();
        if let Some(entry) = state.entries.get_mut(&self.key)
            && entry.ticket == self.ticket
            && entry.completed.is_none()
        {
            entry.previous = None;
            entry.reserved = KEY_RESERVATION;
            entry.completed = Some(Instant::now());
            entry
                .status
                .send_replace(Status::Failed("catalog_unavailable"));
        }
    }
}

#[derive(Default)]
struct Collector {
    guard_target: Option<super::approval_config::Target>,
    servers: BTreeMap<String, Server>,
    cursors: BTreeSet<String>,
    bytes: usize,
    tools: usize,
    pages: usize,
    reserved: usize,
}
fn identifier(v: &Value) -> Result<&str, &'static str> {
    v.as_str()
        .filter(|s| !s.is_empty() && s.len() <= 1024)
        .ok_or("catalog_invalid")
}
impl Collector {
    #[cfg(test)]
    fn page(&mut self, page: Value) -> Result<Option<String>, &'static str> {
        self.page_with_context(page, false)
    }
    fn page_with_context(
        &mut self,
        page: Value,
        process: bool,
    ) -> Result<Option<String>, &'static str> {
        let bytes = serde_json::to_vec(&page)
            .map_err(|_| "catalog_invalid")?
            .len();
        self.bytes = self.bytes.checked_add(bytes).ok_or("catalog_too_large")?;
        self.pages += 1;
        if bytes > PAGE_BYTES || self.bytes > TOTAL_BYTES || self.pages > 64 {
            return Err("catalog_too_large");
        }
        let servers = page
            .get("data")
            .and_then(Value::as_array)
            .ok_or("catalog_invalid")?;
        for server in servers {
            let name = identifier(&server["name"])?;
            if self.servers.contains_key(name) {
                return Err("catalog_invalid");
            }
            let runtime_status = if process && server["runtimeStatus"].is_null() {
                "notStarted"
            } else {
                identifier(&server["runtimeStatus"])?
            };
            if ![
                "notStarted",
                "starting",
                "connected",
                "authenticationRequired",
                "failed",
                "cancelled",
                "disabled",
            ]
            .contains(&runtime_status)
            {
                return Err("catalog_invalid");
            }
            let auth_status = identifier(&server["authStatus"])?;
            if ![
                "unknown",
                "unsupported",
                "notLoggedIn",
                "bearerToken",
                "oAuth",
            ]
            .contains(&auth_status)
            {
                return Err("catalog_invalid");
            }
            let tools = server["tools"].as_object().ok_or("catalog_invalid")?;
            self.tools += tools.len();
            if self.tools > 4096 {
                return Err("catalog_too_large");
            }
            let mut hashes = BTreeMap::new();
            for (tool, definition) in tools {
                if tool.is_empty()
                    || tool.len() > 1024
                    || definition["name"].as_str() != Some(tool)
                    || !definition["inputSchema"].is_object()
                {
                    return Err("catalog_invalid");
                }
                if name == "codex_apps" && tool == super::approval_config::NOTION_TOOL {
                    self.guard_target =
                        super::approval_config::Target::from_definition(name, definition).ok();
                }
                hashes.insert(tool.clone(), crate::control::fingerprint(definition));
            }
            self.reserved += 512
                + name.len()
                + runtime_status.len()
                + auth_status.len()
                + hashes
                    .iter()
                    .map(|(n, h)| 512 + n.capacity() + h.capacity())
                    .sum::<usize>();
            if self.reserved > TOTAL_BYTES - KEY_RESERVATION {
                return Err("catalog_capacity");
            }
            self.servers.insert(
                name.into(),
                Server {
                    runtime_status: runtime_status.into(),
                    auth_status: auth_status.into(),
                    tools: hashes,
                },
            );
        }
        match page.get("nextCursor") {
            Some(Value::Null) => Ok(None),
            Some(Value::String(s))
                if !s.is_empty()
                    && s.len() <= 8192
                    && self.cursors.insert(s.clone())
                    && self.pages < 64 =>
            {
                Ok(Some(s.clone()))
            }
            _ => Err("catalog_invalid"),
        }
    }
}
async fn collect(runtime: &CodexRuntime, thread: &str) -> Result<Snapshot, &'static str> {
    let mut collector = Collector::default();
    let mut args = json!({"detail":"toolsAndAuthOnly","limit":100});
    if thread != "__process__" {
        args["threadId"] = json!(thread);
    }
    loop {
        let page = tokio::time::timeout(
            Duration::from_secs(30),
            runtime.request("mcpServerStatus/list", args.clone()),
        )
        .await
        .map_err(|_| "catalog_timeout")?
        .map_err(|e| {
            let message = e.to_string();
            if message.contains("catalog_too_large") {
                "catalog_too_large"
            } else if message.contains("catalog_invalid") {
                "catalog_invalid"
            } else {
                "catalog_unavailable"
            }
        })?;
        match collector.page_with_context(page, thread == "__process__")? {
            Some(cursor) => args["cursor"] = json!(cursor),
            None => {
                return Ok(Snapshot {
                    guard_target: collector.guard_target,
                    servers: collector.servers,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[test]
    fn only_preparation_catalog_may_omit_runtime_status() {
        let mut value = page("test", Value::Null);
        value["data"][0]
            .as_object_mut()
            .unwrap()
            .remove("runtimeStatus");
        assert_eq!(
            Collector::default().page(value.clone()).unwrap_err(),
            "catalog_invalid"
        );
        let mut collector = Collector::default();
        assert_eq!(collector.page_with_context(value, true).unwrap(), None);
        assert_eq!(collector.servers["test"].runtime_status, "notStarted");
        assert!(collector.servers["test"].available().is_err());
    }
    fn key(n: usize) -> Key {
        Key {
            instance: "i".into(),
            recovery: "r".into(),
            runtime: "runtime".into(),
            thread: n.to_string(),
            config: "c".into(),
        }
    }
    fn page(name: &str, cursor: Value) -> Value {
        json!({"data":[{"name":name,"runtimeStatus":"connected","authStatus":"unsupported","tools":{"read":{"name":"read","inputSchema":{"type":"object"}}}}],"nextCursor":cursor})
    }
    fn snapshot() -> Snapshot {
        Snapshot {
            guard_target: None,
            servers: BTreeMap::new(),
        }
    }
    async fn ready(mut r: watch::Receiver<Status>) -> Status {
        loop {
            let value = r.borrow_and_update().clone();
            if !matches!(value, Status::Loading) {
                return value;
            }
            r.changed().await.unwrap();
        }
    }
    fn epoch(s: Status) -> String {
        match s {
            Status::Ready { epoch, .. } => epoch,
            s => panic!("not ready: {s:?}"),
        }
    }
    #[test]
    fn all_servers_and_large_definitions_survive_collection() {
        let mut c = Collector::default();
        let mut a = page("apps", json!("next"));
        a["data"][0]["tools"]["read"]["description"] = json!("x".repeat(1_100_000));
        assert_eq!(c.page(a).unwrap(), Some("next".into()));
        let mut b = page("other", Value::Null);
        b["data"][0]["runtimeStatus"] = json!("failed");
        assert!(c.page(b).unwrap().is_none());
        assert_eq!(c.servers.len(), 2);
        assert!(c.servers["apps"].available().is_ok());
        assert_eq!(
            c.servers["other"].available(),
            Err("catalog_server_unavailable")
        );
    }
    #[test]
    fn rejects_partial_invalid_duplicate_and_cyclic_catalogs() {
        for mutation in ["nextCursor", "data"] {
            let mut p = page("s", Value::Null);
            p.as_object_mut().unwrap().remove(mutation);
            assert!(Collector::default().page(p).is_err());
        }
        let mut c = Collector::default();
        c.page(page("s", json!("a"))).unwrap();
        assert!(c.page(page("s", Value::Null)).is_err());
        let mut c = Collector::default();
        c.page(page("s", json!("a"))).unwrap();
        assert!(c.page(page("other", json!("a"))).is_err());
        let mut p = page("s", Value::Null);
        p["data"][0]["tools"]["read"]["name"] = json!("other");
        assert!(Collector::default().page(p).is_err());
    }
    #[tokio::test]
    async fn callers_share_one_worker_and_invalidation_discards_late_results() {
        let manager = Manager::default();
        let count = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Semaphore::new(0));
        let mut receivers = vec![];
        for _ in 0..100 {
            let count = count.clone();
            let gate = gate.clone();
            receivers.push(
                manager
                    .start(key(1), move |_| async move {
                        count.fetch_add(1, Ordering::SeqCst);
                        let _permit = gate.acquire().await.unwrap();
                        Ok(snapshot())
                    })
                    .unwrap(),
            );
        }
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        manager.invalidate(&key(1));
        gate.add_permits(1);
        for r in receivers {
            assert!(matches!(
                ready(r).await,
                Status::Failed("catalog_invalidated")
            ));
        }
        assert!(matches!(
            ready(manager.start(key(1), |_| async { Ok(snapshot()) }).unwrap()).await,
            Status::Ready { .. }
        ));
    }
    #[tokio::test]
    async fn concurrent_worker_and_key_capacity_are_bounded() {
        let manager = Manager::default();
        let count = Arc::new(AtomicUsize::new(0));
        let gate = Arc::new(Semaphore::new(0));
        for n in 0..32 {
            let count = count.clone();
            let gate = gate.clone();
            manager
                .start(key(n), move |_| async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    let _permit = gate.acquire().await.unwrap();
                    Ok(snapshot())
                })
                .unwrap();
        }
        tokio::task::yield_now().await;
        assert_eq!(count.load(Ordering::SeqCst), 2);
        assert!(matches!(
            manager.start(key(33), |_| async { Ok(snapshot()) }),
            Err("catalog_capacity")
        ));
        for n in 0..32 {
            manager.release(&key(n));
        }
        tokio::task::yield_now().await;
        assert!(matches!(
            ready(
                manager
                    .start(key(33), |_| async { Ok(snapshot()) })
                    .unwrap()
            )
            .await,
            Status::Ready { .. }
        ));
    }
    #[tokio::test]
    async fn refresh_preserves_epoch_but_failure_never_revives_it() {
        let manager = Manager::default();
        let first =
            epoch(ready(manager.start(key(1), |_| async { Ok(snapshot()) }).unwrap()).await);
        manager
            .state
            .lock()
            .unwrap()
            .entries
            .get_mut(&key(1))
            .unwrap()
            .completed = Some(Instant::now() - Duration::from_secs(31));
        let second =
            epoch(ready(manager.start(key(1), |_| async { Ok(snapshot()) }).unwrap()).await);
        assert_eq!(first, second);
        manager
            .state
            .lock()
            .unwrap()
            .entries
            .get_mut(&key(1))
            .unwrap()
            .completed = Some(Instant::now() - Duration::from_secs(31));
        assert!(matches!(
            ready(
                manager
                    .start(key(1), |_| async { Err("catalog_timeout") })
                    .unwrap()
            )
            .await,
            Status::Failed("catalog_timeout")
        ));
        assert!(matches!(
            ready(
                manager
                    .start(key(1), |_| async { panic!("failure must be shared") })
                    .unwrap()
            )
            .await,
            Status::Failed("catalog_timeout")
        ));
        manager
            .state
            .lock()
            .unwrap()
            .entries
            .get_mut(&key(1))
            .unwrap()
            .completed = Some(Instant::now() - Duration::from_secs(3));
        let third =
            epoch(ready(manager.start(key(1), |_| async { Ok(snapshot()) }).unwrap()).await);
        assert_ne!(first, third);
    }
    #[tokio::test]
    async fn worker_panic_is_not_permanent_loading() {
        let manager = Manager::default();
        assert!(matches!(
            ready(
                manager
                    .start(key(1), |_| async { panic!("injected worker failure") })
                    .unwrap()
            )
            .await,
            Status::Failed("catalog_unavailable")
        ));
    }
}
