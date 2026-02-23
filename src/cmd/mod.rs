mod get;
pub use get::Get;

mod publish;
pub use publish::Publish;

mod set;
pub use set::Set;

mod subscribe;
pub use subscribe::{Subscribe, Unsubscribe};

mod ping;
pub use ping::Ping;

mod unknown;
pub use unknown::Unknown;

use crate::{Connection, Db, Frame, Parse, ParseError, Shutdown};

/// 支持的 Redis 命令枚举
///
/// 对 `Command` 调用的方法会委托给具体的命令实现
#[derive(Debug)]
pub enum Command {
    Get(Get),
    Publish(Publish),
    Set(Set),
    Subscribe(Subscribe),
    Unsubscribe(Unsubscribe),
    Ping(Ping),
    Unknown(Unknown),
}

impl Command {
    /// 从接收到的帧解析命令
    ///
    /// `Frame` 必须表示一个 `mini-redis` 支持的 Redis 命令，并且是数组类型
    ///
    /// # 返回值
    ///
    /// 成功时返回命令值，否则返回 `Err`
    pub fn from_frame(frame: Frame) -> crate::Result<Command> {
        // 帧值用 `Parse` 装饰。`Parse` 提供了类似"光标"的 API，使得解析命令更容易。
        //
        // 帧值必须是数组类型。任何其他帧类型都会导致返回错误。
        let mut parse = Parse::new(frame)?;

        // 所有 Redis 命令都以命令名作为字符串开始。名称会被读取并转换为小写，
        // 以便进行大小写不敏感的匹配。
        let command_name = parse.next_string()?.to_lowercase();

        // 匹配命令名，将其余的解析委托给具体的命令
        let command = match &command_name[..] {
            "get" => Command::Get(Get::parse_frames(&mut parse)?),
            "publish" => Command::Publish(Publish::parse_frames(&mut parse)?),
            "set" => Command::Set(Set::parse_frames(&mut parse)?),
            "subscribe" => Command::Subscribe(Subscribe::parse_frames(&mut parse)?),
            "unsubscribe" => Command::Unsubscribe(Unsubscribe::parse_frames(&mut parse)?),
            "ping" => Command::Ping(Ping::parse_frames(&mut parse)?),
            _ => {
                // 无法识别命令，返回一个 Unknown 命令。
                //
                // 此处调用 `return` 是为了跳过下面的 `finish()` 调用。由于命令无法识别，
                // `Parse` 实例中很可能还有未消费的字段。
                return Ok(Command::Unknown(Unknown::new(command_name)));
            }
        };

        // 检查 `Parse` 值中是否还有未消费的字段。如果有剩余字段，
        // 则表示帧格式不符合预期，返回错误。
        parse.finish()?;

        // 命令已成功解析
        Ok(command)
    }

    /// 将命令应用到指定的 `Db` 实例
    ///
    /// 响应写入到 `dst`。这由服务器调用来执行接收到的命令
    pub(crate) async fn apply(
        self,
        db: &Db,
        dst: &mut Connection,
        shutdown: &mut Shutdown,
    ) -> crate::Result<()> {
        use Command::*;

        match self {
            Get(cmd) => cmd.apply(db, dst).await,
            Publish(cmd) => cmd.apply(db, dst).await,
            Set(cmd) => cmd.apply(db, dst).await,
            Subscribe(cmd) => cmd.apply(db, dst, shutdown).await,
            Ping(cmd) => cmd.apply(dst).await,
            Unknown(cmd) => cmd.apply(dst).await,
            // `Unsubscribe` 不能在此上下文中应用。它只能从 `Subscribe` 命令的上下文中接收。
            Unsubscribe(_) => Err("`Unsubscribe` is unsupported in this context".into()),
        }
    }

    /// 返回命令名称
    pub(crate) fn get_name(&self) -> &str {
        match self {
            Command::Get(_) => "get",
            Command::Publish(_) => "pub",
            Command::Set(_) => "set",
            Command::Subscribe(_) => "subscribe",
            Command::Unsubscribe(_) => "unsubscribe",
            Command::Ping(_) => "ping",
            Command::Unknown(cmd) => cmd.get_name(),
        }
    }
}
