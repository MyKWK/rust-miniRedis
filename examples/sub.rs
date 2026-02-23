//! 订阅 Redis 通道示例
//!
//! 一个简单的客户端，连接到 mini-redis 服务器，订阅 "foo" 和 "bar" 通道，
//! 并等待在这些通道上发布的消息
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
pub async fn main() -> Result<()> {
    // 打开到 mini-redis 地址的连接
    let client = Client::connect("127.0.0.1:6379").await?;

    // 订阅通道 foo
    let mut subscriber = client.subscribe(vec!["foo".into()]).await?;

    // 等待通道 foo 上的消息
    if let Some(msg) = subscriber.next_message().await? {
        println!(
            "got message from the channel: {}; message = {:?}",
            msg.channel, msg.content
        );
    }

    Ok(())
}
