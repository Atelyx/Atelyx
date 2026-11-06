//! 在途调用表：插件发出的请求按 id 在表中配对回执。
//! 表由传输层持有，读端收到响应即按 id 取走对应的等待端。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::oneshot;

use crate::protocol::envelope::RpcError;

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("握手失败：{0}")]
    Handshake(String),
    #[error("契约版本不一致：插件 {plugin}，宿主 {host}")]
    VersionMismatch { plugin: u32, host: u32 },
    #[error("连接异常结束：{0}")]
    Abnormal(String),
}

pub(crate) type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;
