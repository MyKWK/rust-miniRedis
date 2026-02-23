use crate::Frame;

use bytes::Bytes;
use std::{fmt, str, vec};

/// 用于解析命令的工具
///
/// 命令表示为数组帧。帧中的每个条目是一个"token"。`Parse` 使用数组帧初始化，
/// 并提供类似光标的 API。每个命令结构体都包含一个 `parse_frame` 方法，使用
/// `Parse` 来提取其字段
#[derive(Debug)]
pub(crate) struct Parse {
    /// Array frame iterator.
    parts: vec::IntoIter<Frame>,
}

/// 解析帧时遇到的错误
///
/// 只有 `EndOfStream` 错误在运行时处理。所有其他错误都会导致连接被终止
#[derive(Debug)]
pub(crate) enum ParseError {
    /// 由于帧已被完全消耗，尝试提取值失败
    EndOfStream,

    /// 所有其他错误
    Other(crate::Error),
}

impl Parse {
    /// 创建一个新的 `Parse` 来解析 `frame` 的内容
    ///
    /// 如果 `frame` 不是数组帧，则返回 `Err`
        let array = match frame {
            Frame::Array(array) => array,
            frame => return Err(format!("protocol error; expected array, got {:?}", frame).into()),
        };

        Ok(Parse {
            parts: array.into_iter(),
        })
    }

    /// 返回下一个条目。数组帧是帧的数组，因此下一个条目是一个帧
    fn next(&mut self) -> Result<Frame, ParseError> {
        self.parts.next().ok_or(ParseError::EndOfStream)
    }

    /// 以字符串形式返回下一个条目
    ///
    /// 如果下一个条目不能表示为字符串，则返回错误
        match self.next()? {
            // Both `Simple` and `Bulk` representation may be strings. Strings
            // are parsed to UTF-8.
            //
            // While errors are stored as strings, they are considered separate
            // types.
            Frame::Simple(s) => Ok(s),
            Frame::Bulk(data) => str::from_utf8(&data[..])
                .map(|s| s.to_string())
                .map_err(|_| "protocol error; invalid string".into()),
            frame => Err(format!(
                "protocol error; expected simple frame or bulk frame, got {:?}",
                frame
            )
            .into()),
        }
    }

    /// 以原始字节形式返回下一个条目
    ///
    /// 如果下一个条目不能表示为原始字节，则返回错误
        match self.next()? {
            // Both `Simple` and `Bulk` representation may be raw bytes.
            //
            // Although errors are stored as strings and could be represented as
            // raw bytes, they are considered separate types.
            Frame::Simple(s) => Ok(Bytes::from(s.into_bytes())),
            Frame::Bulk(data) => Ok(data),
            frame => Err(format!(
                "protocol error; expected simple frame or bulk frame, got {:?}",
                frame
            )
            .into()),
        }
    }

    /// 以整数形式返回下一个条目
    ///
    /// 这包括 `Simple`、`Bulk` 和 `Integer` 帧类型。`Simple` 和 `Bulk`
    /// 帧类型会被解析
    ///
    /// 如果下一个条目不能表示为整数，则返回错误
        use atoi::atoi;

        const MSG: &str = "protocol error; invalid number";

        match self.next()? {
            // An integer frame type is already stored as an integer.
            Frame::Integer(v) => Ok(v),
            // Simple and bulk frames must be parsed as integers. If the parsing
            // fails, an error is returned.
            Frame::Simple(data) => atoi::<u64>(data.as_bytes()).ok_or_else(|| MSG.into()),
            Frame::Bulk(data) => atoi::<u64>(&data).ok_or_else(|| MSG.into()),
            frame => Err(format!("protocol error; expected int frame but got {:?}", frame).into()),
        }
    }

    /// 确保数组中没有更多条目
    pub(crate) fn finish(&mut self) -> Result<(), ParseError> {
        if self.parts.next().is_none() {
            Ok(())
        } else {
            Err("protocol error; expected end of frame, but there was more".into())
        }
    }
}

impl From<String> for ParseError {
    fn from(src: String) -> ParseError {
        ParseError::Other(src.into())
    }
}

impl From<&str> for ParseError {
    fn from(src: &str) -> ParseError {
        src.to_string().into()
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::EndOfStream => "protocol error; unexpected end of stream".fmt(f),
            ParseError::Other(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for ParseError {}
