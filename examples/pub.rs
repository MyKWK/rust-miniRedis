//! 发布到 Redis 通道示例
//!
//! 一个简单的客户端，连接到 mini-redis 服务器，并在 `foo` 通道上发布一条消息
//!
//! 你可以通过运行以下命令来测试：
//!
//!     cargo run --bin mini-redis-server
//!
//! 然后在另一个终端中运行：
//!
//!     cargo run --example sub
//!
//! 再在另一个终端中运行：
//!
//!     cargo run --example pub

#![warn(rust_2018_idioms)]

use mini_redis::{clients::Client, Result};

#[tokio::main]
async fn main() -> Result<()> {
    // 打开到 mini-redis 地址的连接
    let mut client = Client::connect("127.0.0.1:6379").await?;

    // 在通道 foo 上发布消息 `bar`
    client.publish("foo", "bar".into()).await?;

    Ok(())
}
