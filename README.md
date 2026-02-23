# mini-redis

`mini-redis` 是一个使用 [Tokio](https://tokio.rs) 构建的不完整、符合惯用法的
[Redis](https://redis.io) 客户端和服务器实现。

该项目的目的是提供一个编写 Tokio 应用程序的更大示例。

**免责声明** 请不要在生产环境中使用 mini-redis。该项目旨在作为学习资源，
省略了 Redis 协议的某些部分，因为实现它们不会引入任何新概念。我们不会
因为你的项目需要而添加新功能 —— 请使用功能完整的替代方案。

## 为什么选择 Redis

本项目的主要目标是教授 Tokio。这需要一个具有广泛功能且注重实现简单性的项目。
Redis 作为一种内存数据库，提供了广泛的功能并使用简单的通信协议。广泛的功能
允许在"真实世界"的上下文中演示许多 Tokio 模式。

Redis 通信协议文档可以在[这里](https://redis.io/topics/protocol)找到。

Redis 提供的命令集可以在[这里](https://redis.io/commands)找到。

## 运行

该仓库提供了一个服务器、客户端库和一些用于与服务器交互的客户端可执行文件。

启动服务器：

```
RUST_LOG=debug cargo run --bin mini-redis-server
```

使用 [`tracing`](https://github.com/tokio-rs/tracing) crate 提供结构化日志。
你可以将 `debug` 替换为所需的[日志级别][level]。

[level]: https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives

然后，在另一个终端窗口中，可以执行各种客户端[示例](examples)。例如：

```
cargo run --example hello_world
```

此外，还提供了一个 CLI 客户端用于从终端运行任意命令。在服务器运行的情况下，
以下命令可以工作：

```
cargo run --bin mini-redis-cli set foo bar

cargo run --bin mini-redis-cli get foo
```

## OpenTelemetry

如果你正在运行应用程序的多个实例（例如，在开发云服务时通常如此），
你需要一种方法将所有跟踪数据从主机传输到集中位置。这里有很多选择，
例如 Prometheus、Jaeger、DataDog、Honeycomb、AWS X-Ray 等。

我们利用 OpenTelemetry，因为它是一个开放标准，允许使用单一数据格式
适用于上述所有选项（以及更多）。这消除了供应商锁定的风险，因为你可以
在需要时在提供商之间切换。

### AWS X-Ray 示例

要启用向 X-Ray 发送跟踪，请使用 `otel` 功能：
```
RUST_LOG=debug cargo run --bin mini-redis-server --features otel
```

这将切换 `tracing` 以使用 `tracing-opentelemetry`。你需要
在同一主机上运行 AWSOtelCollector 的副本。

出于演示目的，你可以按照以下文档中记录的设置进行操作：
https://github.com/aws-observability/aws-otel-collector/blob/main/docs/developers/docker-demo.md#run-a-single-aws-otel-collector-instance-in-docker

## 支持的命令

`mini-redis` 目前支持以下命令。

* [PING](https://redis.io/commands/ping)
* [GET](https://redis.io/commands/get)
* [SET](https://redis.io/commands/set)
* [PUBLISH](https://redis.io/commands/publish)
* [SUBSCRIBE](https://redis.io/commands/subscribe)

Redis 通信协议规范可以在[这里](https://redis.io/topics/protocol)找到。

目前尚不支持持久化。

## Tokio 模式

该项目演示了许多有用的模式，包括：

### TCP 服务器

[`server.rs`](src/server.rs) 启动一个 TCP 服务器，接受连接，
并为每个连接生成一个新任务。它优雅地处理 `accept` 错误。

### 客户端库

[`client.rs`](src/clients/client.rs) 展示了如何建模异步客户端。
各种功能以 `async` 方法的形式公开。

### 跨套接字共享状态

服务器维护一个 [`Db`] 实例，该实例可以从所有连接中访问。
[`Db`] 实例管理键值状态以及发布/订阅功能。

[`Db`]: src/db.rs

### 帧处理

[`connection.rs`](src/connection.rs) 和 [`frame.rs`](src/frame.rs) 展示了
如何符合惯用地实现通信协议。该协议使用中间表示（即 `Frame` 结构）进行建模。
`Connection` 接受一个 `TcpStream` 并公开一个发送和接收 `Frame` 值的 API。

### 优雅关闭

服务器实现优雅关闭。使用 [`tokio::signal`] 监听 SIGINT 信号。
一旦收到信号，关闭就开始了。服务器停止接受新连接。现有连接被通知
优雅关闭。进行中的工作完成，然后连接关闭。

[`tokio::signal`]: https://docs.rs/tokio/*/tokio/signal/

### 并发连接限制

服务器使用 [`Semaphore`] 限制最大并发连接数。一旦达到限制，
服务器停止接受新连接，直到现有连接终止。

[`Semaphore`]: https://docs.rs/tokio/*/tokio/sync/struct.Semaphore.html

### 发布/订阅

服务器实现了非平凡的发布/订阅功能。客户端可以订阅多个通道，
并随时更新其订阅。服务器使用每个通道一个[广播通道][broadcast]和
每个连接一个 [`StreamMap`] 来实现此功能。客户端可以向服务器发送
订阅命令以更新活动订阅。

[broadcast]: https://docs.rs/tokio/*/tokio/sync/broadcast/index.html
[`StreamMap`]: https://docs.rs/tokio-stream/*/tokio_stream/struct.StreamMap.html

### 在异步应用程序中使用 `std::sync::Mutex`

服务器使用 `std::sync::Mutex` 而不是 Tokio mutex 来同步对共享状态的访问。
有关更多详细信息，请参阅 [`db.rs`](src/db.rs)。

### 测试依赖时间的异步代码

在 [`tests/server.rs`](tests/server.rs) 中，有键过期的测试。
这些测试依赖于时间的流逝。为了使测试具有确定性，
使用 Tokio 的测试工具模拟时间。

## 贡献

欢迎对 `mini-redis` 进行贡献。请记住，该项目的目标**不是**
与真正的 Redis 实现功能对等，而是使用 Tokio 演示异步 Rust 模式。

只有在有助于演示新模式的情况下，才应添加命令或其他功能。

贡献应附带针对新 Tokio 用户的详细注释。

仅专注于澄清和改进注释的贡献非常受欢迎。

## 许可证

本项目根据 [MIT 许可证](LICENSE) 授权。

### 贡献条款

除非你明确声明，否则你有意提交给 `mini-redis` 以包含在内的任何贡献，
均应按照 MIT 许可，不附带任何额外条款或条件。
