//! 同步绘制面：宿主在一次 draw 里给出的目标面尺寸与同步能力（量算 / Markdown / 主题 / 推送）。
//! 真身是同进程承载的绘制函数表；测试用内存实现。

use atelyx_plugin_protocol::envelope::RpcError;
use atelyx_plugin_protocol::frame::{FrameRect, HitRegion};
use atelyx_plugin_protocol::method::{MarkdownBlock, MeasureTextResult};
use atelyx_plugin_protocol::paint::{DrawCommand, TextFamily, TextWeight};

use super::build::Ui;
use super::theme::Theme;
use super::FrameUi;

/// 一次 draw 期间宿主提供的同步能力面。全部方法都在宿主的重绘线程上同步执行，
/// 实现必须当场返回——这里没有跨线程等待，也不允许在回调里再调宿主能力面。
pub trait DrawSurface {
    /// 目标面的 id（插件注册贡献时用的那个）。
    fn surface_id(&self) -> &str;
    /// 宿主这一拍给该面分配到的逻辑像素尺寸。
    fn size(&self) -> (f32, f32);
    /// 一段文字的排版占位；`max_width` 给定即按该宽度换行。
    fn measure(
        &self,
        text: &str,
        size: f32,
        weight: Option<TextWeight>,
        family: Option<TextFamily>,
        max_width: Option<f32>,
    ) -> MeasureTextResult;
    /// 把 Markdown 原文解析成结构化块（与笔记同一套语义）；解析不出返回空块。
    fn markdown(&self, content: &str) -> Vec<MarkdownBlock>;
    /// 当前主题（字号档 / 圆角档 / 强调色），每拍取一次。
    fn theme(&self) -> Theme;
    /// 推一条绘制指令（按调用顺序叠放）。
    fn command(&self, command: &DrawCommand);
    /// 推一个命中区（同帧内后声明的在上）。
    fn hit(&self, hit: &HitRegion);
    /// 推输入声明：`ime` 为空表示不声明输入法，`fps` 为空表示不推帧拍。
    fn policy(&self, ime: Option<FrameRect>, keys: bool, fps: Option<u16>);
}

/// 宿主交给插件的一次 draw：目标面 + 这一面的控件层状态。
///
/// 借出的控件层由 SDK 持有：输入回调（异步线程）与绘制（宿主重绘线程）共用同一份交互态，
/// 绘制取不到时宿主跳过这一拍，不会阻塞。
pub struct DrawFrame<'a, 'ui> {
    surface: &'a dyn DrawSurface,
    ui: &'ui mut FrameUi,
    /// 这一面没画成（控件层校验失败）：整拍按失败上报，宿主沿用上一拍而不是清面。
    failed: bool,
}

impl<'a, 'ui> DrawFrame<'a, 'ui> {
    pub(crate) fn new(surface: &'a dyn DrawSurface, ui: &'ui mut FrameUi) -> Self {
        Self { surface, ui, failed: false }
    }

    /// 这一面是否没画成。插件即使忽略了 `build` 的错误，这一次绘制也按失败上报。
    pub(crate) fn failed(&self) -> bool {
        self.failed
    }

    /// 目标面的 id。
    pub fn surface_id(&self) -> &str {
        self.surface.surface_id()
    }

    /// 宿主这一拍给该面的尺寸（逻辑像素）。
    pub fn size(&self) -> (f32, f32) {
        self.surface.size()
    }

    /// 宿主的同步能力面：手绘指令、直接量算、取主题都经它。
    pub fn surface(&self) -> &dyn DrawSurface {
        self.surface
    }

    /// 声明这一面的帧拍频率（1–120；`None` = 不推帧拍）。绘制里改也会应用到下一拍的输入声明。
    pub fn set_fps(&mut self, fps: Option<u16>) {
        self.ui.set_fps(fps);
    }

    /// 程序化聚焦输入框（浮层接管键盘、行内编辑这类场景）：须在 `build` 之前调用。
    /// 语义见 [`FrameUi::focus_input`]。
    pub fn focus_input(&mut self, id: &str, value: &str) {
        self.ui.focus_input(id, value);
    }

    /// 用控件层声明这一面。`build` 只读插件状态：改状态归输入回调，改完请调
    /// [`crate::Context::request_redraw`]。
    pub fn build(&mut self, build: impl Fn(&mut Ui<'_, '_>)) -> Result<(), RpcError> {
        match self.ui.draw(self.surface, build) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.failed = true;
                Err(error)
            }
        }
    }
}
