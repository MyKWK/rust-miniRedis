use crate::frame::{self, Frame};

use bytes::{Buf, BytesMut};
use std::io::{self, Cursor};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};
use tokio::net::TcpStream;

/// 从远程对等方发送和接收 `Frame` 值
///
/// 在实现网络协议时，该协议上的消息通常由几个较小的消息组成，称为帧。
/// `Connection` 的目的是在底层 `TcpStream` 上读取和写入帧
///
/// 要读取帧，`Connection` 使用内部缓冲区，该缓冲区会被填充，直到有足够的
/// 字节来创建完整的帧。一旦这种情况发生，`Connection` 就会创建帧并将其返回
/// 给调用者
///
/// 发送帧时，帧首先被编码到写缓冲区中。然后写缓冲区的内容会被写入套接字
#[derive(Debug)]
pub struct Connection {
    // `TcpStream`。它使用 `BufWriter` 装饰，提供写级别的缓冲
    // Tokio 提供的 `BufWriter` 实现对于我们的需求来说已经足够了
    stream: BufWriter<TcpStream>,

    // 用于读取帧的缓冲区
    buffer: BytesMut,
}

impl Connection {
    /// 创建一个新的 `Connection`，由 `socket` 支持。读取和写入缓冲区被初始化
    pub fn new(socket: TcpStream) -> Connection {
        Connection {
            stream: BufWriter::new(socket),
            // 默认使用 4KB 的读取缓冲区。对于 mini redis 的使用场景来说，
            // 这个大小是合适的。但是，实际应用会希望根据其特定使用场景调整此值。
            // 很有可能更大的读取缓冲区会工作得更好。
            buffer: BytesMut::with_capacity(4 * 1024),
        }
    }

    /// 从底层流中读取单个 `Frame` 值
    ///
    /// 该函数会等待直到它检索到足够的数据来解析帧。帧被解析后留在读取缓冲区中的
    /// 任何数据都会保留，供下一次调用 `read_frame` 使用
    ///
    /// # 返回值
    ///
    /// 成功时返回接收到的帧。如果 `TcpStream` 以不会将帧截断的方式关闭，
    /// 则返回 `None`。否则返回错误
    pub async fn read_frame(&mut self) -> crate::Result<Option<Frame>> {
        loop {
            // 尝试从缓冲数据中解析帧。如果已经缓冲了足够的数据，
            // 则返回帧。
            if let Some(frame) = self.parse_frame()? {
                return Ok(Some(frame));
            }

            // 缓冲的数据不足以读取帧。尝试从套接字读取更多数据。
            //
            // 成功时，返回读取的字节数。`0` 表示"流结束"。
            if 0 == self.stream.read_buf(&mut self.buffer).await? {
                // 远程对等方关闭了连接。要成为干净的关闭，
                // 读取缓冲区中应该没有数据。如果有，这意味着对等方在发送帧时
                // 关闭了套接字。
                if self.buffer.is_empty() {
                    return Ok(None);
                } else {
                    return Err("connection reset by peer".into());
                }
            }
        }
    }

    /// 尝试从缓冲区解析帧。如果缓冲区包含足够的数据，则返回帧并从缓冲区中删除数据。
    /// 如果缓冲的数据还不够，则返回 `Ok(None)`。如果缓冲的数据不表示有效的帧，
    /// 则返回 `Err`
    fn parse_frame(&mut self) -> crate::Result<Option<Frame>> {
        use frame::Error::Incomplete;

        // Cursor 用于跟踪缓冲区中的"当前"位置。Cursor 也实现了 `bytes` crate
        // 的 `Buf` trait，它提供了许多处理字节的有用工具。
        let mut buf = Cursor::new(&self.buffer[..]);

        // 第一步是检查是否已经缓冲了足够的数据来解析单个帧。这一步通常比
        // 完整解析帧快得多，并且允许我们跳过分配数据结构来保存帧数据，
        // 除非我们知道已经收到了完整的帧。
        match Frame::check(&mut buf) {
            Ok(_) => {
                // `check` 函数会将游标前进到帧的末尾。由于在调用 `Frame::check` 之前
                // 游标的位置被设置为零，我们可以通过检查游标位置来获取帧的长度。
                let len = buf.position() as usize;

                // 在将游标传递给 `Frame::parse` 之前，将位置重置为零。
                buf.set_position(0);

                // 从缓冲区解析帧。这会分配必要的结构来表示帧并返回帧值。
                //
                // 如果编码的帧表示无效，则返回错误。这应该终止**当前**连接，
                // 但不应影响任何其他已连接的客户端。
                let frame = Frame::parse(&mut buf)?;

                // 从读取缓冲区中丢弃已解析的数据。
                //
                // 当在读取缓冲区上调用 `advance` 时，直到 `len` 的所有数据都会被丢弃。
                // 具体如何工作的细节留给 `BytesMut`。这通常通过移动内部游标来完成，
                // 但也可能通过重新分配和复制数据来完成。
                self.buffer.advance(len);

                // 将解析的帧返回给调用者。
                Ok(Some(frame))
            }
            // 读取缓冲区中没有足够的数据来解析单个帧。我们必须等待从套接字
            // 接收更多数据。从套接字读取将在这个 `match` 之后的语句中完成。
            //
            // 我们不想从这里返回 `Err`，因为这种"错误"是预期的运行时条件。
            Err(Incomplete) => Ok(None),
            // 解析帧时遇到错误。连接现在处于无效状态。从这里返回 `Err` 
            // 将导致连接被关闭。
            Err(e) => Err(e.into()),
        }
    }

    /// 将单个 `Frame` 值写入底层流
    ///
    /// `Frame` 值使用 `AsyncWrite` 提供的各种 `write_*` 函数写入套接字
    /// 直接在 `TcpStream` 上调用这些函数**不**建议，因为这会导致大量的系统调用
    /// 但是，在*缓冲*写流上调用这些函数是可以的。数据会被写入缓冲区。一旦缓冲区
    /// 满了，它就会被刷新到底层套接字
    pub async fn write_frame(&mut self, frame: &Frame) -> io::Result<()> {
        // 数组通过编码每个条目来编码。所有其他帧类型被视为字面量。
        // 目前，mini-redis 无法编码递归帧结构。详见下文。
        match frame {
            Frame::Array(val) => {
                // 编码帧类型前缀。对于数组，它是 `*`。
                self.stream.write_u8(b'*').await?;

                // 编码数组的长度。
                self.write_decimal(val.len() as u64).await?;

                // 迭代并编码数组中的每个条目。
                for entry in &**val {
                    self.write_value(entry).await?;
                }
            }
            // 帧类型是字面量。直接编码值。
            _ => self.write_value(frame).await?,
        }

        // 确保编码的帧被写入套接字。上面的调用是针对缓冲流和写入。
        // 调用 `flush` 会将缓冲区的剩余内容写入套接字。
        self.stream.flush().await
    }

    /// 将帧字面量写入流
    async fn write_value(&mut self, frame: &Frame) -> io::Result<()> {
        match frame {
            Frame::Simple(val) => {
                self.stream.write_u8(b'+').await?;
                self.stream.write_all(val.as_bytes()).await?;
                self.stream.write_all(b"\r\n").await?;
            }
            Frame::Error(val) => {
                self.stream.write_u8(b'-').await?;
                self.stream.write_all(val.as_bytes()).await?;
                self.stream.write_all(b"\r\n").await?;
            }
            Frame::Integer(val) => {
                self.stream.write_u8(b':').await?;
                self.write_decimal(*val).await?;
            }
            Frame::Null => {
                self.stream.write_all(b"$-1\r\n").await?;
            }
            Frame::Bulk(val) => {
                let len = val.len();

                self.stream.write_u8(b'$').await?;
                self.write_decimal(len as u64).await?;
                self.stream.write_all(val).await?;
                self.stream.write_all(b"\r\n").await?;
            }
            // 从值内部编码 `Array` 无法使用递归策略完成。
            // 通常，异步函数不支持递归。Mini-redis 还不需要编码嵌套数组，
            // 所以目前暂时跳过。
            Frame::Array(_val) => unreachable!(),
        }

        Ok(())
    }

    /// 将十进制帧写入流
    async fn write_decimal(&mut self, val: u64) -> io::Result<()> {
        use std::io::Write;

        // 将值转换为字符串
        let mut buf = [0u8; 20];
        let mut buf = Cursor::new(&mut buf[..]);
        write!(&mut buf, "{}", val)?;

        let pos = buf.position() as usize;
        self.stream.write_all(&buf.get_ref()[..pos]).await?;
        self.stream.write_all(b"\r\n").await?;

        Ok(())
    }
}
