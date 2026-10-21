//! 宿主通知的插件侧处理面：一个 trait 承接全部 `host:` 通知，未知通知落到 stderr（失败可见）。

use serde_json::Value;

use crate::context::Context;
use crate::protocol::event::PublishParams;
use crate::protocol::method::{
    CanvasResizedParams, ChatEventParams, CollabMessageParams, DocCloseParams, DocOpenParams,
    ProcessEventParams, ShortcutTriggeredParams, SurfaceAttachedParams, SurfaceDetachedParams,
    ToolCallParams, TrayClickParams, ViewEventParams, WindowClosedParams, WindowIdParams,
};

/// 宿主通知处理面。全部 `host:` 通知按方法名分派；实现只需覆盖关心的项。
/// 通知处理逐个起独立任务（async-trait 盒装 Send future），单个回调卡住不阻塞连接读取；
/// 因此回调内允许再经 [`Context`] 调宿主（响应由读任务继续消费，不会互等）。
/// 顺序约定：`on_ready` 之前的宿主通知可能先于它执行，且回调之间无先后保证——
/// 需要初始化状态的回调必须容忍「尚未 ready」的情形。
#[async_trait::async_trait]
pub trait PluginHandler: Send + Sync + 'static {
    /// 握手完成后调用一次：提交初始界面树、声明订阅都应在这里做。
    async fn on_ready(&self, ctx: &Context);

    async fn on_view_event(&self, _ctx: &Context, _params: ViewEventParams) {}
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
    /// 画布节点的像素尺寸变化（`host:canvas.resized`）：按新尺寸对齐绘制坐标与滚动范围。
    async fn on_canvas_resized(&self, _ctx: &Context, _params: CanvasResizedParams) {}
    /// 用户点击了本插件的某个托盘条目（`host:tray.click`）。
    async fn on_tray_click(&self, _ctx: &Context, _params: TrayClickParams) {}
    /// 同空间的对等插件发来消息（`host:collab.message`）。
    async fn on_collab_message(&self, _ctx: &Context, _params: CollabMessageParams) {}
    /// 对话事件流（`host:ai.chat.event`）：快照节流拍 + 终态 + 命名结果。
    async fn on_chat_event(&self, _ctx: &Context, _params: ChatEventParams) {}

    /// 工具调用（`host:ai.toolCall`）：须用同一 `callId` 经 [`Context::tool_result`] 回报结果。
    /// 默认回报「未处理」错误，宿主不必等到调用超时。
    async fn on_tool_call(&self, ctx: &Context, params: ToolCallParams) {
        let _ = ctx
            .tool_result(&params.call_id, "插件未处理工具调用", true)
            .await;
    }

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
        m::view::HOST_EVENT => typed!(ViewEventParams, |p| handler.on_view_event(ctx, p)),
        m::view::HOST_ATTACHED => typed!(SurfaceAttachedParams, |p| handler.on_surface_attached(ctx, p)),
        m::view::HOST_DETACHED => typed!(SurfaceDetachedParams, |p| handler.on_surface_detached(ctx, p)),
        event::HOST_PUBLISH => typed!(PublishParams, |p| handler.on_host_event(ctx, p)),
        m::process::HOST_EVENT => typed!(ProcessEventParams, |p| handler.on_process_event(ctx, p)),
        m::shortcut::HOST_TRIGGERED => typed!(ShortcutTriggeredParams, |p| handler.on_shortcut(ctx, p)),
        m::doc::HOST_OPEN => typed!(DocOpenParams, |p| handler.on_doc_open(ctx, p)),
        m::doc::HOST_CLOSE => typed!(DocCloseParams, |p| handler.on_doc_close(ctx, p)),
        m::window::HOST_CLOSED => typed!(WindowClosedParams, |p| handler.on_window_closed(ctx, p)),
        m::window::HOST_HIDDEN => typed!(WindowIdParams, |p| handler.on_window_hidden(ctx, p)),
        m::canvas::HOST_RESIZED => typed!(CanvasResizedParams, |p| handler.on_canvas_resized(ctx, p)),
        m::tray::HOST_CLICK => typed!(TrayClickParams, |p| handler.on_tray_click(ctx, p)),
        m::collab::HOST_MESSAGE => typed!(CollabMessageParams, |p| handler.on_collab_message(ctx, p)),
        m::ai::HOST_CHAT_EVENT => typed!(ChatEventParams, |p| handler.on_chat_event(ctx, p)),
        m::ai::HOST_TOOL_CALL => typed!(ToolCallParams, |p| handler.on_tool_call(ctx, p)),
        other => handler.on_unknown_notification(ctx, other, params).await,
    }
}
