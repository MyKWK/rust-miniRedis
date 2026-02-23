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
            // Default to a 4KB read buffer. For the use case of mini redis,
            // this is fine. However, real applications will want to tune this
            // value to their specific use case. There is a high likelihood that
            // a larger read buffer will work better.
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
            // Attempt to parse a frame from the buffered data. If enough data
            // has been buffered, the frame is returned.
            if let Some(frame) = self.parse_frame()? {
                return Ok(Some(frame));
            }

            // There is not enough buffered data to read a frame. Attempt to
            // read more data from the socket.
            //
            // On success, the number of bytes is returned. `0` indicates "end
            // of stream".
            if 0 == self.stream.read_buf(&mut self.buffer).await? {
                // The remote closed the connection. For this to be a clean
                // shutdown, there should be no data in the read buffer. If
                // there is, this means that the peer closed the socket while
                // sending a frame.
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

        // Cursor is used to track the "current" location in the
        // buffer. Cursor also implements `Buf` from the `bytes` crate
        // which provides a number of helpful utilities for working
        // with bytes.
        let mut buf = Cursor::new(&self.buffer[..]);

        // The first step is to check if enough data has been buffered to parse
        // a single frame. This step is usually much faster than doing a full
        // parse of the frame, and allows us to skip allocating data structures
        // to hold the frame data unless we know the full frame has been
        // received.
        match Frame::check(&mut buf) {
            Ok(_) => {
                // The `check` function will have advanced the cursor until the
                // end of the frame. Since the cursor had position set to zero
                // before `Frame::check` was called, we obtain the length of the
                // frame by checking the cursor position.
                let len = buf.position() as usize;

                // Reset the position to zero before passing the cursor to
                // `Frame::parse`.
                buf.set_position(0);

                // Parse the frame from the buffer. This allocates the necessary
                // structures to represent the frame and returns the frame
                // value.
                //
                // If the encoded frame representation is invalid, an error is
                // returned. This should terminate the **current** connection
                // but should not impact any other connected client.
                let frame = Frame::parse(&mut buf)?;

                // Discard the parsed data from the read buffer.
                //
                // When `advance` is called on the read buffer, all of the data
                // up to `len` is discarded. The details of how this works is
                // left to `BytesMut`. This is often done by moving an internal
                // cursor, but it may be done by reallocating and copying data.
                self.buffer.advance(len);

                // Return the parsed frame to the caller.
                Ok(Some(frame))
            }
            // There is not enough data present in the read buffer to parse a
            // single frame. We must wait for more data to be received from the
            // socket. Reading from the socket will be done in the statement
            // after this `match`.
            //
            // We do not want to return `Err` from here as this "error" is an
            // expected runtime condition.
            Err(Incomplete) => Ok(None),
            // An error was encountered while parsing the frame. The connection
            // is now in an invalid state. Returning `Err` from here will result
            // in the connection being closed.
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
        // Arrays are encoded by encoding each entry. All other frame types are
        // considered literals. For now, mini-redis is not able to encode
        // recursive frame structures. See below for more details.
        match frame {
            Frame::Array(val) => {
                // Encode the frame type prefix. For an array, it is `*`.
                self.stream.write_u8(b'*').await?;

                // Encode the length of the array.
                self.write_decimal(val.len() as u64).await?;

                // Iterate and encode each entry in the array.
                for entry in &**val {
                    self.write_value(entry).await?;
                }
            }
            // The frame type is a literal. Encode the value directly.
            _ => self.write_value(frame).await?,
        }

        // Ensure the encoded frame is written to the socket. The calls above
        // are to the buffered stream and writes. Calling `flush` writes the
        // remaining contents of the buffer to the socket.
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
            // Encoding an `Array` from within a value cannot be done using a
            // recursive strategy. In general, async fns do not support
            // recursion. Mini-redis has not needed to encode nested arrays yet,
            // so for now it is skipped.
            Frame::Array(_val) => unreachable!(),
        }

        Ok(())
    }

    /// 将十进制帧写入流
    async fn write_decimal(&mut self, val: u64) -> io::Result<()> {
        use std::io::Write;

        // Convert the value to a string
        let mut buf = [0u8; 20];
        let mut buf = Cursor::new(&mut buf[..]);
        write!(&mut buf, "{}", val)?;

        let pos = buf.position() as usize;
        self.stream.write_all(&buf.get_ref()[..pos]).await?;
        self.stream.write_all(b"\r\n").await?;

        Ok(())
    }
}
