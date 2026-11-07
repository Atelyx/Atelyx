//! 在途调用表：插件发出的请求按 id 在表中配对回执，读端收到响应即按 id 取走对应的等待端。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::oneshot;

use crate::protocol::envelope::RpcError;

pub(crate) type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;
