//! 系统剪贴板的唯一出口：文本与图片的读写都在同一常驻线程、同一个实例上串行执行。
//!
//! 实例常驻是 X11 的硬约束：剪切板内容由实例的服务线程应答读取请求，实例一释放内容即失效。
//! 同一时刻系统剪贴板只允许一个线程操作，另开实例会与常驻实例争用并可能永远等不到回执，
//! 所以调用方只递请求，不自己持剪贴板实例。请求经通道交给该线程，结果同步回给调用方
//! （调用方在线程池上等，不占 UI 线程）。

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::OnceLock;

enum Request {
    WriteText {
        text: String,
        reply: Sender<Result<(), String>>,
    },
    ReadText {
        reply: Sender<Result<Option<String>, String>>,
    },
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

/// 写入文本（调用方应在线程池线程执行）。
pub fn write_text(text: String) -> Result<(), String> {
    send(|reply| Request::WriteText { text, reply })
}

/// 读剪贴板中的文本（没有文本 = `None`）。调用方应在线程池线程执行。
pub fn read_text() -> Result<Option<String>, String> {
    send(|reply| Request::ReadText { reply })
}

/// 写入一张 RGBA 图片（调用方应在线程池线程执行）。
pub fn write_image(width: u32, height: u32, rgba: Vec<u8>) -> Result<(), String> {
    send(|reply| Request::WriteImage {
        width: width as usize,
        height: height as usize,
        rgba,
        reply,
    })
}

/// 读剪贴板中的图片（返回宽、高与 RGBA8 像素；剪贴板里没有图片 = `None`）。
/// 调用方应在线程池线程执行。
pub fn read_image() -> Result<Option<(usize, usize, Vec<u8>)>, String> {
    send(|reply| Request::ReadImage { reply })
}

/// 复制类动作的界面入口：交给剪贴板线程且不等待回执，失败留日志（调用方无需结果）。
pub fn write_text_task(text: String) -> iced::Task<()> {
    iced::Task::perform(
        async move {
            match tokio::task::spawn_blocking(move || write_text(text)).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!("写入剪贴板失败：{error}"),
                Err(error) => eprintln!("剪贴板任务失败：{error}"),
            }
        },
        |_| (),
    )
}

fn send<T>(
    request: impl FnOnce(Sender<Result<T, String>>) -> Request,
) -> Result<T, String> {
    let tx = dispatcher().as_ref().ok_or_else(|| "剪贴板线程不可用".to_string())?;
    let (reply, reply_rx) = mpsc::channel();
    tx.send(request(reply))
        .map_err(|_| "剪贴板线程不可用".to_string())?;
    reply_rx.recv().map_err(|_| "剪贴板线程无响应".to_string())?
}

fn dispatcher() -> &'static Option<Sender<Request>> {
    static TX: OnceLock<Option<Sender<Request>>> = OnceLock::new();
    TX.get_or_init(|| spawn(Arboard::default()))
}

/// 启动线程独占一个后端实例；线程退出（通道关闭）前实例一直存活。
fn spawn<B: Backend + 'static>(backend: B) -> Option<Sender<Request>> {
    let (tx, rx) = mpsc::channel::<Request>();
    std::thread::Builder::new()
        .name("clipboard".into())
        .spawn(move || serve(backend, rx))
        .ok()
        .map(|_| tx)
}

fn serve<B: Backend>(mut backend: B, rx: Receiver<Request>) {
    for request in rx {
        match request {
            Request::WriteText { text, reply } => {
                let _ = reply.send(backend.set_text(&text));
            }
            Request::ReadText { reply } => {
                let _ = reply.send(backend.get_text());
            }
            Request::WriteImage { width, height, rgba, reply } => {
                let _ = reply.send(backend.set_image(width, height, rgba));
            }
            Request::ReadImage { reply } => {
                let _ = reply.send(backend.get_image());
            }
        }
    }
}

/// 剪贴板后端。测试替身据此表达「实例被占用、回执不回」。
trait Backend: Send {
    fn set_text(&mut self, text: &str) -> Result<(), String>;
    fn get_text(&mut self) -> Result<Option<String>, String>;
    fn set_image(&mut self, width: usize, height: usize, rgba: Vec<u8>) -> Result<(), String>;
    fn get_image(&mut self) -> Result<Option<(usize, usize, Vec<u8>)>, String>;
}

#[derive(Default)]
struct Arboard {
    clipboard: Option<arboard::Clipboard>,
}

impl Arboard {
    /// 首次操作时才连接系统剪贴板：连接失败不留在状态里，下一次请求重新尝试。
    fn instance(&mut self) -> Result<&mut arboard::Clipboard, String> {
        if self.clipboard.is_none() {
            self.clipboard =
                Some(arboard::Clipboard::new().map_err(|e| format!("系统剪贴板不可用：{e}"))?);
        }
        Ok(self.clipboard.as_mut().expect("剪贴板实例已初始化"))
    }
}

impl Backend for Arboard {
    fn set_text(&mut self, text: &str) -> Result<(), String> {
        self.instance()?
            .set_text(text)
            .map_err(|e| format!("写入剪贴板失败：{e}"))
    }

    fn get_text(&mut self) -> Result<Option<String>, String> {
        match self.instance()?.get_text() {
            Ok(text) => Ok(Some(text)),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(format!("读取剪贴板文本失败：{e}")),
        }
    }

    fn set_image(&mut self, width: usize, height: usize, rgba: Vec<u8>) -> Result<(), String> {
        if width == 0 || height == 0 || rgba.len() != width * height * 4 {
            return Err("图片像素数据不完整".to_string());
        }
        self.instance()?
            .set_image(arboard::ImageData { width, height, bytes: rgba.into() })
            .map_err(|e| format!("写入剪贴板失败：{e}"))
    }

    fn get_image(&mut self) -> Result<Option<(usize, usize, Vec<u8>)>, String> {
        match self.instance()?.get_image() {
            Ok(image) => Ok(Some((image.width, image.height, image.bytes.into_owned()))),
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(format!("读取剪贴板图片失败：{e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Condvar, Mutex};
    use std::time::{Duration, Instant};

    /// 系统剪贴板：同一时刻只允许一个实例操作。第二个实例被占用时回执永不返回，
    /// 替身按超时给出可读错误，避免测试挂死。
    struct FakeOs {
        state: Mutex<FakeState>,
        released: Condvar,
        owners: AtomicUsize,
    }

    struct FakeState {
        text: Option<String>,
        image: Option<(usize, usize, Vec<u8>)>,
    }

    impl FakeOs {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                state: Mutex::new(FakeState { text: None, image: None }),
                released: Condvar::new(),
                owners: AtomicUsize::new(0),
            })
        }

        fn open(self: &Arc<Self>) -> FakeBackend {
            self.owners.fetch_add(1, Ordering::SeqCst);
            FakeBackend { os: self.clone() }
        }

        fn owners(&self) -> usize {
            self.owners.load(Ordering::SeqCst)
        }

        /// 被占用（存在第二个实例）时等到超时，模拟回执永远不回来。
        fn block_while_shared(&self) -> Result<(), String> {
            let mut state = self.state.lock().unwrap();
            if self.owners() > 1 {
                state = self
                    .released
                    .wait_timeout(state, Duration::from_millis(500))
                    .expect("互斥锁不应中毒")
                    .0;
                if self.owners() > 1 {
                    return Err("剪贴板实例被占用，回执未返回".to_string());
                }
            }
            drop(state);
            Ok(())
        }
    }

    struct FakeBackend {
        os: Arc<FakeOs>,
    }

    impl Drop for FakeBackend {
        fn drop(&mut self) {
            self.os.owners.fetch_sub(1, Ordering::SeqCst);
            self.os.released.notify_all();
        }
    }

    impl Backend for FakeBackend {
        fn set_text(&mut self, text: &str) -> Result<(), String> {
            self.os.block_while_shared()?;
            self.os.state.lock().unwrap().text = Some(text.to_string());
            Ok(())
        }

        fn get_text(&mut self) -> Result<Option<String>, String> {
            self.os.block_while_shared()?;
            Ok(self.os.state.lock().unwrap().text.clone())
        }

        fn set_image(&mut self, width: usize, height: usize, rgba: Vec<u8>) -> Result<(), String> {
            self.os.block_while_shared()?;
            self.os.state.lock().unwrap().image = Some((width, height, rgba));
            Ok(())
        }

        fn get_image(&mut self) -> Result<Option<(usize, usize, Vec<u8>)>, String> {
            self.os.block_while_shared()?;
            Ok(self.os.state.lock().unwrap().image.clone())
        }
    }

    fn call<T>(
        tx: &Sender<Request>,
        request: impl FnOnce(Sender<Result<T, String>>) -> Request,
    ) -> Result<T, String> {
        let (reply, reply_rx) = mpsc::channel();
        tx.send(request(reply)).expect("剪贴板线程应仍在运行");
        reply_rx.recv().expect("剪贴板线程应有回执")
    }

    /// 复现缺陷：常驻实例存活时另开第二个实例，写文本要等到超时也拿不到回执。
    #[test]
    fn second_instance_is_starved_by_the_persistent_owner() {
        let os = FakeOs::new();
        let first = os.open();
        let mut peer = os.open();
        let started = Instant::now();
        let result = peer.set_text("复现");
        assert!(result.is_err(), "第二个实例不应能操作剪贴板");
        assert!(
            started.elapsed() >= Duration::from_millis(400),
            "被占用时应先等回执再失败，实际 {:?}",
            started.elapsed()
        );
        drop(first);
    }

    /// 单一出口：文本与图片都走常驻实例，先读图片不挡住后续写文本，实例数始终为一。
    #[test]
    fn one_instance_serves_text_and_image() {
        let os = FakeOs::new();
        let tx = spawn(os.open()).expect("剪贴板线程应能启动");
        assert_eq!(call(&tx, |reply| Request::ReadImage { reply }), Ok(None));
        assert_eq!(
            call(&tx, |reply| Request::WriteText { text: "粘贴".into(), reply }),
            Ok(())
        );
        assert_eq!(
            call(&tx, |reply| Request::ReadText { reply }),
            Ok(Some("粘贴".to_string()))
        );
        assert_eq!(os.owners(), 1, "文本与图片共用同一实例");
    }
}
