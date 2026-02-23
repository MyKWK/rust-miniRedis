//! Hello world 示例
//!
//! 一个简单的客户端，连接到 mini-redis 服务器，将键 "hello" 设置为值 "world"，
//! 然后从服务器获取它
//!
//! 你可以通过运行以下命令来测试：
//!
//!     cargo run --bin mini-redis-server
//!
//! 然后在另一个终端中运行：
//!
//!     cargo run --example hello_world

#![warn(rust_2018_idioms)]

use mini_redis::{clients::Client, Result};

#[tokio::main]
pub async fn main() -> Result<()> {
    // 打开到 mini-redis 地址的连接
    let mut client = Client::connect("127.0.0.1:6379").await?;

    // 将键 "hello" 设置为值 "world"
    client.set("hello", "world".into()).await?;

    // 获取键 "hello"
    let result = client.get("hello").await?;

    println!("got value from the server; success={:?}", result.is_some());

    Ok(())
}
