use tokio::sync::{broadcast, Notify};
use tokio::time::{self, Duration, Instant};

use bytes::Bytes;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};
use tracing::debug;

/// `Db` 实例的包装器。它的存在是为了通过通知后台清理任务在
/// 此结构体被删除时关闭，从而允许对 `Db` 进行有序清理
#[derive(Debug)]
pub(crate) struct DbDropGuard {
    /// The `Db` instance that will be shut down when this `DbDropGuard` struct
    /// is dropped.
    db: Db,
}

/// 在所有连接之间共享的服务器状态
///
/// `Db` 包含一个存储键/值数据的 `HashMap` 以及用于活动发布/订阅通道的
/// 所有 `broadcast::Sender` 值
///
/// `Db` 实例是共享状态的句柄。克隆 `Db` 是浅拷贝，只会增加原子引用计数
///
/// 当创建 `Db` 值时，会生成一个后台任务。该任务用于在请求的持续时间过去后
/// 使值过期。该任务会一直运行，直到所有 `Db` 实例都被删除，此时任务终止
#[derive(Debug, Clone)]
pub(crate) struct Db {
    /// Handle to shared state. The background task will also have an
    /// `Arc<Shared>`.
    shared: Arc<Shared>,
}

#[derive(Debug)]
struct Shared {
    /// 共享状态由互斥锁保护。这是 `std::sync::Mutex` 而不是 Tokio 互斥锁。
    /// 这是因为在持有互斥锁时没有执行异步操作。此外，临界区非常小
    ///
    /// Tokio 互斥锁主要用于需要在 `.await` yield 点之间持有锁的情况。
    /// 所有其他情况通常都最适合使用 std 互斥锁。如果临界区不包含任何
    /// 异步操作但是很长（CPU 密集型或执行阻塞操作），则整个操作，包括
    /// 等待互斥锁，都被视为"阻塞"操作，应该使用 `tokio::task::spawn_blocking`
    state: Mutex<State>,

    /// 通知处理条目过期的后台任务。后台任务等待被通知，然后检查过期的值
    /// 或关闭信号
    background_task: Notify,
}

#[derive(Debug)]
struct State {
    /// 键值数据。我们没有尝试做任何花哨的事情，所以
    /// `std::collections::HashMap` 就可以正常工作
    entries: HashMap<String, Entry>,

    /// 发布/订阅键空间。Redis 为键值和发布/订阅使用**单独**的键空间。
    /// `mini-redis` 通过使用单独的 `HashMap` 来处理这个问题
    pub_sub: HashMap<String, broadcast::Sender<Bytes>>,

    /// 跟踪键的 TTL（生存时间）
    ///
    /// 使用 `BTreeSet` 来按过期时间维护排序的过期项。这允许后台任务遍历
    /// 此映射以找到下一个过期的值
    ///
    /// 虽然极不可能，但有可能在同一时刻创建多个过期项。因此，`Instant`
    /// 对于键来说是不够的。使用唯一键（`String`）来打破这些平局
    expirations: BTreeSet<(Instant, String)>,

    /// 当 Db 实例关闭时为 true。当所有 `Db` 值被删除时会发生这种情况。
    /// 将其设置为 `true` 会向后台任务发出退出信号
    shutdown: bool,
}

/// 键值存储中的条目
#[derive(Debug)]
struct Entry {
    /// 存储的数据
    data: Bytes,

    /// 条目过期并应从数据库中删除的时刻
    expires_at: Option<Instant>,
}

impl DbDropGuard {
    /// 创建一个新的 `DbDropGuard`，包装一个 `Db` 实例。当此对象被删除时，
    /// `Db` 的清理任务将被关闭
    pub(crate) fn new() -> DbDropGuard {
        DbDropGuard { db: Db::new() }
    }

    /// 获取共享数据库。内部这是一个 `Arc`，所以克隆只增加引用计数
    pub(crate) fn db(&self) -> Db {
        self.db.clone()
    }
}

impl Drop for DbDropGuard {
    fn drop(&mut self) {
        // Signal the 'Db' instance to shut down the task that purges expired keys
        self.db.shutdown_purge_task();
    }
}

impl Db {
    /// 创建一个新的空 `Db` 实例。分配共享状态并生成后台任务来管理键过期
    ///
    /// 键关联的值
    ///
    /// 如果没有值与键关联，则返回 `None`。这可能是由于从未为键分配过值，
    /// 或者之前分配的值已过期
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                entries: HashMap::new(),
                pub_sub: HashMap::new(),
                expirations: BTreeSet::new(),
                shutdown: false,
            }),
            background_task: Notify::new(),
        });

        // Start the background task.
        tokio::spawn(purge_expired_tasks(shared.clone()));

        Db { shared }
    }

    /// 获取与键关联的值
    ///
    /// 如果没有值与键关联，则返回 `None`。这可能是由于从未为键分配过值，
    /// 或者之前分配的值已过期
    pub(crate) fn get(&self, key: &str) -> Option<Bytes> {
        // Acquire the lock, get the entry and clone the value.
        //
        // Because data is stored using `Bytes`, a clone here is a shallow
        // clone. Data is not copied.
        let state = self.shared.state.lock().unwrap();
        state.entries.get(key).map(|entry| entry.data.clone())
    }

    /// 设置与键关联的值以及可选的过期持续时间
    ///
    /// 如果已经有一个值与键关联，它将被移除
    pub(crate) fn set(&self, key: String, value: Bytes, expire: Option<Duration>) {
        let mut state = self.shared.state.lock().unwrap();

        // If this `set` becomes the key that expires **next**, the background
        // task needs to be notified so it can update its state.
        //
        // Whether or not the task needs to be notified is computed during the
        // `set` routine.
        let mut notify = false;

        let expires_at = expire.map(|duration| {
            // `Instant` at which the key expires.
            let when = Instant::now() + duration;

            // Only notify the worker task if the newly inserted expiration is the
            // **next** key to evict. In this case, the worker needs to be woken up
            // to update its state.
            notify = state
                .next_expiration()
                .map(|expiration| expiration > when)
                .unwrap_or(true);

            when
        });

        // Insert the entry into the `HashMap`.
        let prev = state.entries.insert(
            key.clone(),
            Entry {
                data: value,
                expires_at,
            },
        );

        // If there was a value previously associated with the key **and** it
        // had an expiration time. The associated entry in the `expirations` map
        // must also be removed. This avoids leaking data.
        if let Some(prev) = prev {
            if let Some(when) = prev.expires_at {
                // clear expiration
                state.expirations.remove(&(when, key.clone()));
            }
        }

        // Track the expiration. If we insert before remove that will cause bug
        // when current `(when, key)` equals prev `(when, key)`. Remove then insert
        // can avoid this.
        if let Some(when) = expires_at {
            state.expirations.insert((when, key));
        }

        // Release the mutex before notifying the background task. This helps
        // reduce contention by avoiding the background task waking up only to
        // be unable to acquire the mutex due to this function still holding it.
        drop(state);

        if notify {
            // Finally, only notify the background task if it needs to update
            // its state to reflect a new expiration.
            self.shared.background_task.notify_one();
        }
    }

    /// 返回请求通道的 `Receiver`
    ///
    /// 返回的 `Receiver` 用于接收由 `PUBLISH` 命令广播的值
    pub(crate) fn subscribe(&self, key: String) -> broadcast::Receiver<Bytes> {
        use std::collections::hash_map::Entry;

        // Acquire the mutex
        let mut state = self.shared.state.lock().unwrap();

        // If there is no entry for the requested channel, then create a new
        // broadcast channel and associate it with the key. If one already
        // exists, return an associated receiver.
        match state.pub_sub.entry(key) {
            Entry::Occupied(e) => e.get().subscribe(),
            Entry::Vacant(e) => {
                // No broadcast channel exists yet, so create one.
                //
                // The channel is created with a capacity of `1024` messages. A
                // message is stored in the channel until **all** subscribers
                // have seen it. This means that a slow subscriber could result
                // in messages being held indefinitely.
                //
                // When the channel's capacity fills up, publishing will result
                // in old messages being dropped. This prevents slow consumers
                // from blocking the entire system.
                let (tx, rx) = broadcast::channel(1024);
                e.insert(tx);
                rx
            }
        }
    }

    /// 向通道发布消息。返回监听该通道的订阅者数量
    pub(crate) fn publish(&self, key: &str, value: Bytes) -> usize {
        let state = self.shared.state.lock().unwrap();

        state
            .pub_sub
            .get(key)
            // On a successful message send on the broadcast channel, the number
            // of subscribers is returned. An error indicates there are no
            // receivers, in which case, `0` should be returned.
            .map(|tx| tx.send(value).unwrap_or(0))
            // If there is no entry for the channel key, then there are no
            // subscribers. In this case, return `0`.
            .unwrap_or(0)
    }

    /// 向清理后台任务发送关闭信号。这由 `DbShutdown` 的 `Drop` 实现调用
    fn shutdown_purge_task(&self) {
        // The background task must be signaled to shut down. This is done by
        // setting `State::shutdown` to `true` and signalling the task.
        let mut state = self.shared.state.lock().unwrap();
        state.shutdown = true;

        // Drop the lock before signalling the background task. This helps
        // reduce lock contention by ensuring the background task doesn't
        // wake up only to be unable to acquire the mutex.
        drop(state);
        self.shared.background_task.notify_one();
    }
}

impl Shared {
    /// 清理所有过期的键并返回**下一个**键将过期的时刻。后台任务将睡眠
    /// 直到该时刻
    fn purge_expired_keys(&self) -> Option<Instant> {
        let mut state = self.state.lock().unwrap();

        if state.shutdown {
            // The database is shutting down. All handles to the shared state
            // have dropped. The background task should exit.
            return None;
        }

        // This is needed to make the borrow checker happy. In short, `lock()`
        // returns a `MutexGuard` and not a `&mut State`. The borrow checker is
        // not able to see "through" the mutex guard and determine that it is
        // safe to access both `state.expirations` and `state.entries` mutably,
        // so we get a "real" mutable reference to `State` outside of the loop.
        let state = &mut *state;

        // Find all keys scheduled to expire **before** now.
        let now = Instant::now();

        while let Some(&(when, ref key)) = state.expirations.iter().next() {
            if when > now {
                // Done purging, `when` is the instant at which the next key
                // expires. The worker task will wait until this instant.
                return Some(when);
            }

            // The key expired, remove it
            state.entries.remove(key);
            state.expirations.remove(&(when, key.clone()));
        }

        None
    }

    /// 如果数据库正在关闭，返回 `true`
    ///
    /// 当所有 `Db` 值都被删除时设置 `shutdown` 标志，表示无法再访问共享状态
    fn is_shutdown(&self) -> bool {
        self.state.lock().unwrap().shutdown
    }
}

impl State {
    fn next_expiration(&self) -> Option<Instant> {
        self.expirations
            .iter()
            .next()
            .map(|expiration| expiration.0)
    }
}

/// 由后台任务执行的例程
///
/// 等待被通知。收到通知时，从共享状态句柄中清除任何过期的键。
/// 如果设置了 `shutdown`，则终止任务
async fn purge_expired_tasks(shared: Arc<Shared>) {
    // If the shutdown flag is set, then the task should exit.
    while !shared.is_shutdown() {
        // Purge all keys that are expired. The function returns the instant at
        // which the **next** key will expire. The worker should wait until the
        // instant has passed then purge again.
        if let Some(when) = shared.purge_expired_keys() {
            // Wait until the next key expires **or** until the background task
            // is notified. If the task is notified, then it must reload its
            // state as new keys have been set to expire early. This is done by
            // looping.
            tokio::select! {
                _ = time::sleep_until(when) => {}
                _ = shared.background_task.notified() => {}
            }
        } else {
            // There are no keys expiring in the future. Wait until the task is
            // notified.
            shared.background_task.notified().await;
        }
    }

    debug!("Purge background task shut down")
}
