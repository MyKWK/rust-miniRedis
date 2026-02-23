//! 最小 Redis 服务器实现
//!
//! 提供一个异步的 `run` 函数，用于侦听传入连接，并为每个连接生成一个任务

use crate::{Command, Connection, Db, DbDropGuard, Shutdown};

use std::future::Future;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc, Semaphore};
use tokio::time::{self, Duration};
use tracing::{debug, error, info, instrument};

/// 服务器侦听器状态。在 `run` 调用中创建。它包括一个 `run` 方法，
/// 该方法执行 TCP 侦听并初始化每连接状态
#[derive(Debug)]
struct Listener {
    /// 共享数据库句柄
    ///
    /// 包含键/值存储以及用于发布/订阅的广播通道
    ///
    /// 这持有围绕 `Arc` 的包装器。可以检索内部 `Db` 并将其传递到每连接状态（`Handler`）
    db_holder: DbDropGuard,

    /// 由 `run` 调用者提供的 TCP 侦听器
    listener: TcpListener,

    /// 限制最大连接数
    ///
    /// 使用 `Semaphore` 来限制最大连接数。在尝试接受新连接之前，会从信号量
    /// 获取一个许可。如果没有可用许可，侦听器会等待一个
    ///
    /// 当处理程序完成处理连接时，许可会返回给信号量
    limit_connections: Arc<Semaphore>,

    /// 向所有活动连接广播关闭信号
    ///
    /// 初始的 `shutdown` 触发器由 `run` 调用者提供。服务器负责优雅地关闭活动连接。
    /// 当生成连接任务时，会传递一个广播接收器句柄。当启动优雅关闭时，
    /// 会通过 broadcast::Sender 发送一个 `()` 值。每个活动连接都会收到它，
    /// 达到安全的终止状态，并完成任务
    notify_shutdown: broadcast::Sender<()>,

    /// 用作优雅关闭过程的一部分，以等待客户端连接完成处理
    ///
    /// 当所有 `Sender` 句柄超出范围时，Tokio 通道会关闭。当通道关闭时，
    /// 接收器会收到 `None`。这用于检测所有连接处理程序已完成。
    /// 当初始化连接处理程序时，会分配 `shutdown_complete_tx` 的一个克隆。
    /// 当侦听器关闭时，它会删除此 `shutdown_complete_tx` 字段持有的发送器。
    /// 一旦所有处理程序任务完成，`Sender` 的所有克隆也会被删除。这导致
    /// `shutdown_complete_rx.recv()` 以 `None` 完成。此时，退出服务器进程是安全的
    shutdown_complete_tx: mpsc::Sender<()>,
}

/// 每连接处理程序。从 `connection` 读取请求并将命令应用到 `db`
#[derive(Debug)]
struct Handler {
    /// 共享数据库句柄
    ///
    /// 当从 `connection` 收到命令时，它被应用到 `db`。命令实现在 `cmd` 模块中。
    /// 每个命令都需要与 `db` 交互才能完成工作
    db: Db,

    /// TCP 连接，使用使用缓冲 `TcpStream` 实现的 redis 协议编码器/解码器装饰
    ///
    /// 当 `Listener` 收到传入连接时，`TcpStream` 被传递给 `Connection::new`，
    /// 它初始化关联的缓冲区。`Connection` 允许处理程序在"帧"级别操作，
    /// 并将字节级协议解析细节封装在 `Connection` 中
    connection: Connection,

    /// 侦听关闭通知
    ///
    /// `broadcast::Receiver` 的包装器，与 `Listener` 中的发送器配对。
    /// 连接处理程序处理来自连接的请求，直到对等方断开连接**或**
    /// 从 `shutdown` 收到关闭通知。在后一种情况下，继续为对等方处理的任何
    /// 进行中的工作，直到它达到安全状态，此时终止连接
    shutdown: Shutdown,

    /// 不直接使用。相反，当 `Handler` 被删除时...？
    _shutdown_complete: mpsc::Sender<()>,
}

/// Maximum number of concurrent connections the redis server will accept.
///
/// When this limit is reached, the server will stop accepting connections until
/// an active connection terminates.
///
/// A real application will want to make this value configurable, but for this
/// example, it is hard coded.
///
/// This is also set to a pretty low value to discourage using this in
/// production (you'd think that all the disclaimers would make it obvious that
/// this is not a serious project... but I thought that about mini-http as
/// well).
const MAX_CONNECTIONS: usize = 250;

/// Run the mini-redis server.
///
/// Accepts connections from the supplied listener. For each inbound connection,
/// a task is spawned to handle that connection. The server runs until the
/// `shutdown` future completes, at which point the server shuts down
/// gracefully.
///
/// `tokio::signal::ctrl_c()` can be used as the `shutdown` argument. This will
/// listen for a SIGINT signal.
pub async fn run(listener: TcpListener, shutdown: impl Future) {
    // When the provided `shutdown` future completes, we must send a shutdown
    // message to all active connections. We use a broadcast channel for this
    // purpose. The call below ignores the receiver of the broadcast pair, and when
    // a receiver is needed, the subscribe() method on the sender is used to create
    // one.
    let (notify_shutdown, _) = broadcast::channel(1);
    let (shutdown_complete_tx, mut shutdown_complete_rx) = mpsc::channel(1);

    // Initialize the listener state
    let mut server = Listener {
        listener,
        db_holder: DbDropGuard::new(),
        limit_connections: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        notify_shutdown,
        shutdown_complete_tx,
    };

    // Concurrently run the server and listen for the `shutdown` signal. The
    // server task runs until an error is encountered, so under normal
    // circumstances, this `select!` statement runs until the `shutdown` signal
    // is received.
    //
    // `select!` statements are written in the form of:
    //
    // ```
    // <result of async op> = <async op> => <step to perform with result>
    // ```
    //
    // All `<async op>` statements are executed concurrently. Once the **first**
    // op completes, its associated `<step to perform with result>` is
    // performed.
    //
    // The `select!` macro is a foundational building block for writing
    // asynchronous Rust. See the API docs for more details:
    //
    // https://docs.rs/tokio/*/tokio/macro.select.html
    tokio::select! {
        res = server.run() => {
            // If an error is received here, accepting connections from the TCP
            // listener failed multiple times and the server is giving up and
            // shutting down.
            //
            // Errors encountered when handling individual connections do not
            // bubble up to this point.
            if let Err(err) = res {
                error!(cause = %err, "failed to accept");
            }
        }
        _ = shutdown => {
            // The shutdown signal has been received.
            info!("shutting down");
        }
    }

    // Extract the `shutdown_complete` receiver and transmitter
    // explicitly drop `shutdown_transmitter`. This is important, as the
    // `.await` below would otherwise never complete.
    let Listener {
        shutdown_complete_tx,
        notify_shutdown,
        ..
    } = server;

    // When `notify_shutdown` is dropped, all tasks which have `subscribe`d will
    // receive the shutdown signal and can exit
    drop(notify_shutdown);
    // Drop final `Sender` so the `Receiver` below can complete
    drop(shutdown_complete_tx);

    // Wait for all active connections to finish processing. As the `Sender`
    // handle held by the listener has been dropped above, the only remaining
    // `Sender` instances are held by connection handler tasks. When those drop,
    // the `mpsc` channel will close and `recv()` will return `None`.
    let _ = shutdown_complete_rx.recv().await;
}

impl Listener {
    /// Run the server
    ///
    /// Listen for inbound connections. For each inbound connection, spawn a
    /// task to process that connection.
    ///
    /// # Errors
    ///
    /// Returns `Err` if accepting returns an error. This can happen for a
    /// number reasons that resolve over time. For example, if the underlying
    /// operating system has reached an internal limit for max number of
    /// sockets, accept will fail.
    ///
    /// The process is not able to detect when a transient error resolves
    /// itself. One strategy for handling this is to implement a back off
    /// strategy, which is what we do here.
    async fn run(&mut self) -> crate::Result<()> {
        info!("accepting inbound connections");

        loop {
            // Wait for a permit to become available
            //
            // `acquire_owned` returns a permit that is bound to the semaphore.
            // When the permit value is dropped, it is automatically returned
            // to the semaphore.
            //
            // `acquire_owned()` returns `Err` when the semaphore has been
            // closed. We don't ever close the semaphore, so `unwrap()` is safe.
            let permit = self
                .limit_connections
                .clone()
                .acquire_owned()
                .await
                .unwrap();

            // Accept a new socket. This will attempt to perform error handling.
            // The `accept` method internally attempts to recover errors, so an
            // error here is non-recoverable.
            let socket = self.accept().await?;

            // Create the necessary per-connection handler state.
            let mut handler = Handler {
                // Get a handle to the shared database.
                db: self.db_holder.db(),

                // Initialize the connection state. This allocates read/write
                // buffers to perform redis protocol frame parsing.
                connection: Connection::new(socket),

                // Receive shutdown notifications.
                shutdown: Shutdown::new(self.notify_shutdown.subscribe()),

                // Notifies the receiver half once all clones are
                // dropped.
                _shutdown_complete: self.shutdown_complete_tx.clone(),
            };

            // Spawn a new task to process the connections. Tokio tasks are like
            // asynchronous green threads and are executed concurrently.
            tokio::spawn(async move {
                // Process the connection. If an error is encountered, log it.
                if let Err(err) = handler.run().await {
                    error!(cause = ?err, "connection error");
                }
                // Move the permit into the task and drop it after completion.
                // This returns the permit back to the semaphore.
                drop(permit);
            });
        }
    }

    /// Accept an inbound connection.
    ///
    /// Errors are handled by backing off and retrying. An exponential backoff
    /// strategy is used. After the first failure, the task waits for 1 second.
    /// After the second failure, the task waits for 2 seconds. Each subsequent
    /// failure doubles the wait time. If accepting fails on the 6th try after
    /// waiting for 64 seconds, then this function returns with an error.
    async fn accept(&mut self) -> crate::Result<TcpStream> {
        let mut backoff = 1;

        // Try to accept a few times
        loop {
            // Perform the accept operation. If a socket is successfully
            // accepted, return it. Otherwise, save the error.
            match self.listener.accept().await {
                Ok((socket, _)) => return Ok(socket),
                Err(err) => {
                    if backoff > 64 {
                        // Accept has failed too many times. Return the error.
                        return Err(err.into());
                    }
                }
            }

            // Pause execution until the back off period elapses.
            time::sleep(Duration::from_secs(backoff)).await;

            // Double the back off
            backoff *= 2;
        }
    }
}

impl Handler {
    /// Process a single connection.
    ///
    /// Request frames are read from the socket and processed. Responses are
    /// written back to the socket.
    ///
    /// Currently, pipelining is not implemented. Pipelining is the ability to
    /// process more than one request concurrently per connection without
    /// interleaving frames. See for more details:
    /// https://redis.io/topics/pipelining
    ///
    /// When the shutdown signal is received, the connection is processed until
    /// it reaches a safe state, at which point it is terminated.
    #[instrument(skip(self))]
    async fn run(&mut self) -> crate::Result<()> {
        // As long as the shutdown signal has not been received, try to read a
        // new request frame.
        while !self.shutdown.is_shutdown() {
            // While reading a request frame, also listen for the shutdown
            // signal.
            let maybe_frame = tokio::select! {
                res = self.connection.read_frame() => res?,
                _ = self.shutdown.recv() => {
                    // If a shutdown signal is received, return from `run`.
                    // This will result in the task terminating.
                    return Ok(());
                }
            };

            // If `None` is returned from `read_frame()` then the peer closed
            // the socket. There is no further work to do and the task can be
            // terminated.
            let frame = match maybe_frame {
                Some(frame) => frame,
                None => return Ok(()),
            };

            // Convert the redis frame into a command struct. This returns an
            // error if the frame is not a valid redis command or it is an
            // unsupported command.
            let cmd = Command::from_frame(frame)?;

            // Logs the `cmd` object. The syntax here is a shorthand provided by
            // the `tracing` crate. It can be thought of as similar to:
            //
            // ```
            // debug!(cmd = format!("{:?}", cmd));
            // ```
            //
            // `tracing` provides structured logging, so information is "logged"
            // as key-value pairs.
            debug!(?cmd);

            // Perform the work needed to apply the command. This may mutate the
            // database state as a result.
            //
            // The connection is passed into the apply function which allows the
            // command to write response frames directly to the connection. In
            // the case of pub/sub, multiple frames may be send back to the
            // peer.
            cmd.apply(&self.db, &mut self.connection, &mut self.shutdown)
                .await?;
        }

        Ok(())
    }
}
