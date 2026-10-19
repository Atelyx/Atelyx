//! 图片写系统剪贴板（灯箱「复制图片」）。
//!
//! 剪贴板实例由专用线程独占并存活到进程结束：X11 的剪切板内容由实例的服务线程应答
//! 读取请求，实例一释放内容即失效。写入命令经通道交给该线程，结果同步回给调用方
//! （调用方在线程池上等，不占 UI 线程）。

use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;

struct Request {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
    reply: Sender<Result<(), String>>,
}

/// 写入一张 RGBA 图片（调用方应在线程池线程执行）。
pub fn write_image(width: u32, height: u32, rgba: Vec<u8>) -> Result<(), String> {
    let tx = dispatcher().as_ref().ok_or_else(|| "剪贴板线程不可用".to_string())?;
    let (reply, reply_rx) = mpsc::channel();
    tx.send(Request { width: width as usize, height: height as usize, rgba, reply })
        .map_err(|_| "剪贴板线程不可用".to_string())?;
    reply_rx.recv().map_err(|_| "剪贴板线程无响应".to_string())?
}

fn dispatcher() -> &'static Option<Sender<Request>> {
    static TX: OnceLock<Option<Sender<Request>>> = OnceLock::new();
    TX.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Request>();
        std::thread::Builder::new()
            .name("image-clipboard".into())
            .spawn(move || {
                // 初始化失败不留在状态里：下一次写入重新尝试
                let mut clipboard: Option<arboard::Clipboard> = None;
                for request in rx {
                    let result = set_image(
                        &mut clipboard,
                        request.width,
                        request.height,
                        request.rgba,
                    );
                    let _ = request.reply.send(result);
                }
            })
            .ok()
            .map(|_| tx)
    })
}

fn set_image(
    clipboard: &mut Option<arboard::Clipboard>,
    width: usize,
    height: usize,
    rgba: Vec<u8>,
) -> Result<(), String> {
    if width == 0 || height == 0 || rgba.len() != width * height * 4 {
        return Err("图片像素数据不完整".to_string());
    }
    if clipboard.is_none() {
        *clipboard =
            Some(arboard::Clipboard::new().map_err(|e| format!("系统剪贴板不可用：{e}"))?);
    }
    let instance = clipboard.as_mut().expect("剪贴板实例已初始化");
    instance
        .set_image(arboard::ImageData { width, height, bytes: rgba.into() })
        .map_err(|e| format!("写入剪贴板失败：{e}"))
}
