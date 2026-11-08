//! 宿主通知的插件侧处理面：一个 trait 承接全部 `host:` 通知，未知通知落到 stderr（失败可见）。

use serde_json::Value;

use crate::context::Context;
use crate::protocol::event::PublishParams;
use crate::protocol::frame::{FrameInputParams, FrameTickParams};
use crate::protocol::method::{
    ChatEventParams, CollabMessageParams, DocCloseParams, DocOpenParams, ProcessEventParams,
    ShortcutTriggeredParams, SurfaceAttachedParams, SurfaceDetachedParams, ToolCallParams,
    TrayClickParams, WindowClosedParams, WindowIdParams,
};
use crate::protocol::service::{self, ChangedParams, HostCallParams};

/// 宿主通知处理面。全部 `host:` 通知按方法名分派；实现只需覆盖关心的项。
/// 回调按通知到达顺序逐个执行：读线程只入队，单个 worker 顺序消费，因此同一份状态不必
/// 自己加锁；代价是单个回调卡住会挡住后续通知（弹对话框 / 长请求这类阻塞调用要么容忍
/// 整体暂停，要么在回调里另起任务自己驱动）。回调里仍可再经 [`Context`] 调宿主——响应
/// 配对在读线程上，与回调不会互等。
/// 顺序约定：`on_ready` 之前的宿主通知可能先于它执行——需要初始化状态的回调必须容忍
/// 「尚未 ready」的情形。
#[async_trait::async_trait]
pub trait PluginHandler: Send + Sync + 'static {
    /// 握手完成后调用一次：注册贡献点、提交首帧、声明订阅都应在这里做。
    async fn on_ready(&self, ctx: &Context);

    /// 面重新进入布局（面板被添加 / 插件重启后重发）：补交该面当前的一帧。
    async fn on_surface_attached(&self, _ctx: &Context, _params: SurfaceAttachedParams) {}
    async fn on_surface_detached(&self, _ctx: &Context, _params: SurfaceDetachedParams) {}
    async fn on_host_event(&self, _ctx: &Context, _params: PublishParams) {}
    async fn on_process_event(&self, _ctx: &Context, _params: ProcessEventParams) {}
    async fn on_shortcut(&self, _ctx: &Context, _params: ShortcutTriggeredParams) {}
    async fn on_doc_open(&self, _ctx: &Context, _params: DocOpenParams) {}
    async fn on_doc_close(&self, _ctx: &Context, _params: DocCloseParams) {}
    async fn on_window_closed(&self, _ctx: &Context, _params: WindowClosedParams) {}
    /// 窗口因失焦被收起（`host:window.hidden`）：需要再唤起时经 [`Context::window_show`] 重新显示。
    async fn on_window_hidden(&self, _ctx: &Context, _params: WindowIdParams) {}
    /// 用户点击了本插件的某个托盘条目（`host:tray.click`）。
    async fn on_tray_click(&self, _ctx: &Context, _params: TrayClickParams) {}
    /// 同空间的对等插件发来消息（`host:collab.message`）。
    async fn on_collab_message(&self, _ctx: &Context, _params: CollabMessageParams) {}
    /// 对话事件流（`host:ai.chat.event`）：快照节流拍 + 终态 + 命名结果。
    async fn on_chat_event(&self, _ctx: &Context, _params: ChatEventParams) {}
    /// 整面帧的原始输入（`host:frame.input`）：指针 / 滚轮 / 键盘 / 输入法 / 焦点 / 尺寸。
    /// 状态变化后由插件自行 `submit_frame`。
    async fn on_frame_input(&self, _ctx: &Context, _params: FrameInputParams) {}
    /// 整面帧的帧拍（`host:frame.tick`）：帧声明了 `fps` 才有，两拍之间应提交新内容。
    async fn on_frame_tick(&self, _ctx: &Context, _params: FrameTickParams) {}

    /// 工具调用（`host:ai.toolCall`）：须用同一 `callId` 经 [`Context::tool_result`] 回报结果。
    /// 默认回报「未处理」错误，宿主不必等到调用超时。
    async fn on_tool_call(&self, ctx: &Context, params: ToolCallParams) {
        let _ = ctx
            .tool_result(&params.call_id, "插件未处理工具调用", true)
            .await;
    }

    /// 服务调用（`host:service.call`）：本插件是这个服务的活动提供者，须用同一 `callId` 经
    /// [`Context::service_result`] 回报结果、经 [`Context::service_error`] 回报失败。
    /// 默认回报「本插件不提供该服务」，宿主不必等到调用超时。
    async fn on_service_call(&self, ctx: &Context, params: HostCallParams) {
        let _ = ctx
            .service_error(
                &params.call_id,
                &format!("{}：本插件不提供该服务", service::error::UNKNOWN_METHOD),
            )
            .await;
    }

    /// 某服务的活动提供者变了（`host:service.changed`，广播给全部运行中插件）。
    /// 需要据此更新「本插件提供的服务是否生效」的界面时覆盖它；默认忽略。
    async fn on_service_changed(&self, _ctx: &Context, _params: ChangedParams) {}

    /// 未知通知：默认写 stderr（宿主把插件 stderr 归入诊断输出）。
    async fn on_unknown_notification(&self, _ctx: &Context, method: &str, params: Value) {
        eprintln!("插件收到未知的宿主通知：{method}：{params}");
    }
}

/// 按方法名分派一条宿主通知。params 解析失败按未知通知处理并注明原因（不静默丢弃）。
pub(crate) async fn dispatch<H: PluginHandler + ?Sized>(
    handler: &H,
    ctx: &Context,
    method: &str,
    params: Value,
) {
    use crate::protocol::event;
    use crate::protocol::method as m;

    macro_rules! typed {
        ($ty:ty, $call:expr) => {
            match serde_json::from_value::<$ty>(params) {
                Ok(p) => $call(p).await,
                Err(e) => {
                    let note = format!("params 不可解析：{e}");
                    handler.on_unknown_notification(ctx, method, Value::String(note)).await
                }
            }
        };
    }

    match method {
        m::view::HOST_ATTACHED => typed!(SurfaceAttachedParams, |p| handler.on_surface_attached(ctx, p)),
        m::view::HOST_DETACHED => typed!(SurfaceDetachedParams, |p| handler.on_surface_detached(ctx, p)),
        event::HOST_PUBLISH => typed!(PublishParams, |p| handler.on_host_event(ctx, p)),
        m::process::HOST_EVENT => typed!(ProcessEventParams, |p| handler.on_process_event(ctx, p)),
        m::shortcut::HOST_TRIGGERED => typed!(ShortcutTriggeredParams, |p| handler.on_shortcut(ctx, p)),
        m::doc::HOST_OPEN => typed!(DocOpenParams, |p| handler.on_doc_open(ctx, p)),
        m::doc::HOST_CLOSE => typed!(DocCloseParams, |p| handler.on_doc_close(ctx, p)),
        m::window::HOST_CLOSED => typed!(WindowClosedParams, |p| handler.on_window_closed(ctx, p)),
        m::window::HOST_HIDDEN => typed!(WindowIdParams, |p| handler.on_window_hidden(ctx, p)),
        m::frame::HOST_INPUT => typed!(FrameInputParams, |p| handler.on_frame_input(ctx, p)),
        m::frame::HOST_TICK => typed!(FrameTickParams, |p| handler.on_frame_tick(ctx, p)),
        m::tray::HOST_CLICK => typed!(TrayClickParams, |p| handler.on_tray_click(ctx, p)),
        m::collab::HOST_MESSAGE => typed!(CollabMessageParams, |p| handler.on_collab_message(ctx, p)),
        m::ai::HOST_CHAT_EVENT => typed!(ChatEventParams, |p| handler.on_chat_event(ctx, p)),
        m::ai::HOST_TOOL_CALL => typed!(ToolCallParams, |p| handler.on_tool_call(ctx, p)),
        service::method::HOST_CALL => typed!(HostCallParams, |p| handler.on_service_call(ctx, p)),
        service::method::HOST_CHANGED => {
            typed!(ChangedParams, |p| handler.on_service_changed(ctx, p))
        }
        other => handler.on_unknown_notification(ctx, other, params).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    /// 只关心帧输入的处理器。
    struct InputWatcher(std::sync::Mutex<Vec<String>>);

    #[async_trait::async_trait]
    impl PluginHandler for InputWatcher {
        async fn on_ready(&self, _ctx: &Context) {}
        async fn on_frame_input(&self, _ctx: &Context, params: FrameInputParams) {
            assert_eq!(params.surface_id, "panel");
            let crate::protocol::frame::FrameInput::PointerDown { x, hit, .. } = params.input else {
                panic!("应为指针按下");
            };
            self.0.lock().unwrap().push(format!("{x}:{}", hit.unwrap_or_default()));
        }
    }

    /// 只关心帧拍的处理器。
    struct TickWatcher(std::sync::atomic::AtomicUsize);

    #[async_trait::async_trait]
    impl PluginHandler for TickWatcher {
        async fn on_ready(&self, _ctx: &Context) {}
        async fn on_frame_tick(&self, _ctx: &Context, params: FrameTickParams) {
            assert_eq!(params.frame, 3);
            assert_eq!(params.surface_id, "panel");
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn frame_input_maps_to_its_own_callback() {
        let (tx, _rx) = mpsc::channel(8);
        let ctx = Context::new(tx);
        let watcher = InputWatcher(std::sync::Mutex::new(Vec::new()));
        dispatch(
            &watcher,
            &ctx,
            crate::protocol::method::frame::HOST_INPUT,
            serde_json::json!({
                "surfaceId": "panel",
                "input": { "kind": "pointerDown", "x": 12.0, "y": 8.0, "button": "left", "hit": "inc" },
            }),
        )
        .await;
        assert_eq!(watcher.0.lock().unwrap().as_slice(), ["12:inc"]);
    }

    #[tokio::test]
    async fn frame_tick_maps_to_its_own_callback() {
        let (tx, _rx) = mpsc::channel(8);
        let ctx = Context::new(tx);
        let watcher = TickWatcher(std::sync::atomic::AtomicUsize::new(0));
        dispatch(
            &watcher,
            &ctx,
            crate::protocol::method::frame::HOST_TICK,
            serde_json::json!({ "surfaceId": "panel", "frame": 3, "deltaMs": 16 }),
        )
        .await;
        assert_eq!(watcher.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unparsable_params_go_to_the_unknown_hook() {
        struct Recorder(std::sync::Mutex<Vec<String>>);

        #[async_trait::async_trait]
        impl PluginHandler for Recorder {
            async fn on_ready(&self, _ctx: &Context) {}
            async fn on_unknown_notification(&self, _ctx: &Context, method: &str, params: Value) {
                self.0.lock().unwrap().push(format!("{method}:{params}"));
            }
        }

        let (tx, _rx) = mpsc::channel(8);
        let ctx = Context::new(tx);
        let recorder = Recorder(std::sync::Mutex::new(Vec::new()));
        dispatch(
            &recorder,
            &ctx,
            crate::protocol::method::frame::HOST_INPUT,
            serde_json::json!({ "nope": 1 }),
        )
        .await;
        let seen = recorder.0.lock().unwrap();
        assert!(seen[0].contains("不可解析"), "解析失败须留痕：{seen:?}");
    }
}
