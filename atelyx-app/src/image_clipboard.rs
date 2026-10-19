//! 系统剪贴板的图片读写（灯箱「复制图片」、插件 `clipboard.readImage`）。
//!
//! 剪贴板实例由专用线程独占并存活到进程结束：X11 的剪切板内容由实例的服务线程应答
//! 读取请求，实例一释放内容即失效；且同一时刻系统剪贴板只能被一个线程打开，全部
//! 图片读写串行化到同一线程上。命令经通道交给该线程，结果同步回给调用方
//! （调用方在线程池上等，不占 UI 线程）。

use std::sync::mpsc::{self, Sender};
use std::sync::OnceLock;

enum Request {
    WriteImage {
        width: usize,
        height: usize,
        rgba: Vec<u8>,
        reply: Sender<Result<(), String>>,
    },
    ReadImage {
        reply: Sender<Result<Option<(usize, usize, Vec<u8>)>, String>>,
    },
}

/// 写入一张 RGBA 图片（调用方应在线程池线程执行）。
pub fn write_image(width: u32, height: u32, rgba: Vec<u8>) -> Result<(), String> {
    let tx = dispatcher().as_ref().ok_or_else(|| "剪贴板线程不可用".to_string())?;
    let (reply, reply_rx) = mpsc::channel();
    tx.send(Request::WriteImage { width: width as usize, height: height as usize, rgba, reply })
        .map_err(|_| "剪贴板线程不可用".to_string())?;
    reply_rx.recv().map_err(|_| "剪贴板线程无响应".to_string())?
}

/// 读剪贴板中的图片（返回宽、高与 RGBA8 像素；剪贴板里没有图片 = `None`）。
/// 调用方应在线程池线程执行。
pub fn read_image() -> Result<Option<(usize, usize, Vec<u8>)>, String> {
    let tx = dispatcher().as_ref().ok_or_else(|| "剪贴板线程不可用".to_string())?;
    let (reply, reply_rx) = mpsc::channel();
    tx.send(Request::ReadImage { reply })
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
                // 初始化失败不留在状态里：下一次读写重新尝试
                let mut clipboard: Option<arboard::Clipboard> = None;
                for request in rx {
                    match request {
                        Request::WriteImage { width, height, rgba, reply } => {
                            let _ = reply.send(set_image(&mut clipboard, width, height, rgba));
                        }
                        Request::ReadImage { reply } => {
                            let _ = reply.send(get_image(&mut clipboard));
                        }
                    }
                }
            })
            .ok()
            .map(|_| tx)
    })
}

fn instance(clipboard: &mut Option<arboard::Clipboard>) -> Result<&mut arboard::Clipboard, String> {
    if clipboard.is_none() {
        *clipboard =
            Some(arboard::Clipboard::new().map_err(|e| format!("系统剪贴板不可用：{e}"))?);
    }
    Ok(clipboard.as_mut().expect("剪贴板实例已初始化"))
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
    instance(clipboard)?
        .set_image(arboard::ImageData { width, height, bytes: rgba.into() })
        .map_err(|e| format!("写入剪贴板失败：{e}"))
}

fn get_image(
    clipboard: &mut Option<arboard::Clipboard>,
) -> Result<Option<(usize, usize, Vec<u8>)>, String> {
    match instance(clipboard)?.get_image() {
        Ok(image) => Ok(Some((image.width, image.height, image.bytes.into_owned()))),
        Err(arboard::Error::ContentNotAvailable) => Ok(None),
        Err(e) => Err(format!("读取剪贴板图片失败：{e}")),
    }
}
