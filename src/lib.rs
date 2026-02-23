//! 一个最小化（即非常不完整）的 Redis 服务器和客户端实现
//!
//! 本项目的目的是提供一个使用 Tokio 构建的异步 Rust 项目的更大示例
//! 请不要尝试在生产环境中运行它……我是认真的
//!
//! # 布局
//!
//! 该库的结构设计使其可以与教程配合使用。其中有些模块是公开的，但在"真正"的
//! redis 客户端库中可能不会公开
//!
//! 主要组件包括：
//!
//! * `server`：Redis 服务器实现。包含一个单独的 `run` 函数，该函数接受一个
//!   `TcpListener` 并开始接受 redis 客户端连接
//!
//! * `clients/client`：异步 Redis 客户端实现。演示如何使用 Tokio 构建客户端
//!
//! * `cmd`：支持的 Redis 命令的实现
//!
//! * `frame`：表示单个 Redis 协议帧。帧被用作"命令"和字节表示之间的中间表示

pub mod clients;
pub use clients::{BlockingClient, BufferedClient, Client};

pub mod cmd;
pub use cmd::Command;

mod connection;
pub use connection::Connection;

pub mod frame;
pub use frame::Frame;

mod db;
use db::Db;
use db::DbDropGuard;

mod parse;
use parse::{Parse, ParseError};

pub mod server;

mod shutdown;
use shutdown::Shutdown;

/// Redis 服务器监听的默认端口
///
/// 如果未指定端口，则使用此端口
pub const DEFAULT_PORT: u16 = 6379;

/// 大多数函数返回的错误类型
///
/// 在编写真实应用程序时，可能需要考虑专门的错误处理 crate 或将错误类型定义为
/// 原因的 `enum`。但是，对于我们的示例，使用 boxed `std::error::Error` 就足够了
///
/// 出于性能原因，在任何热路径中都避免装箱。例如，在 `parse` 中，定义了一个自定义
/// 错误 `enum`。这是因为在正常执行期间，在套接字上接收到部分帧时会遇到并处理错误
/// `std::error::Error` 已为 `parse::Error` 实现，允许将其转换为 `Box<dyn std::error::Error>`
pub type Error = Box<dyn std::error::Error + Send + Sync>;

/// mini-redis 操作的专用 `Result` 类型
///
/// 这是为了方便而定义的
pub type Result<T> = std::result::Result<T, Error>;
