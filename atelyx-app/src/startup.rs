//! 启动加载屏：步骤清单、就绪闸门与超时；闸门全部到达后以不透明覆盖层等待
//! 主窗可见插件面按真实尺寸出帧，再原地撤屏。

use std::path::PathBuf;
use std::time::{Duration, Instant};

use iced::widget::{canvas, column, container, image, mouse_area, row, space, Stack};
use iced::{Element, Length, Point, Renderer, Size};

use crate::ui::palette as pal;
use crate::{app_icon, ui, App, Message};

/// 步骤清单区宽度（与扫光条同宽）。
const LIST_W: f32 = 460.0;
/// 步骤清单区高度（封顶，行高紧凑排布）。
const LIST_H: f32 = 156.0;
/// 单行步骤高度（11 行恰好铺满清单区）。
const ROW_H: f32 = 14.0;
/// 应用图标边长。
const LOGO: f32 = 64.0;
/// 扫光条高度。
const SWEEP_H: f32 = 2.0;
/// 扫光段占轨道宽度的比例。
const SWEEP_SEG: f32 = 0.28;
/// 扫光相位每拍前进步长（33ms 一拍、约 1.1s 走完一轮）。
const SWEEP_STEP: f32 = 0.03;
/// 转圈图标每拍旋转弧度。
const SPIN_STEP: f32 = 0.19;

/// boot 同步阶段只收集、首帧后执行的启动计划。
#[derive(Default)]
pub struct BootPlan {
    /// 全局配置损坏等启动提示（首帧后补发通知）。
    pub notice: Option<String>,
    /// 启动自动进入的本地仓库根。
    pub auto_vault: Option<PathBuf>,
    /// 启动自动进入的协作空间（服务器地址 / 空间 id / 显示名）。
    pub space: Option<(String, String, String)>,
    /// 启动静默检查更新。
    pub auto_update: bool,
}

impl BootPlan {
    /// 取出并清空（首帧只执行一次）。
    pub fn take(&mut self) -> BootPlan {
        std::mem::take(self)
    }
}

/// 各闸门的兜底上限：到点即放行。实测冷启动（插件同步/扫描/启动 + 进入空间并读回配置）
/// 最慢约 1.9s，取约 2 倍余量；文档恢复含空间 HTTP 读盘，故另给 5s。
const PLUGINS_LIMIT: Duration = Duration::from_millis(4000);
const VAULT_LIMIT: Duration = Duration::from_millis(4000);
const DOCS_LIMIT: Duration = Duration::from_millis(5000);
/// 总上限：闸门组合的等待上限；覆盖层等待插件面出帧的阶段不设上限。
const STARTUP_TOTAL: Duration = Duration::from_millis(8000);

/// 加载步骤（序即展示序；文案即界面文本）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    GlobalConfig,
    UiState,
    Plugins,
    OpenRepo,
    OpenSpace,
    SpaceSession,
    SpaceConnect,
    VaultConfig,
    FileTree,
    AiSessions,
    RestoreDocs,
    Windows,
}

impl Step {
    pub fn label(self) -> &'static str {
        match self {
            Step::GlobalConfig => "读取全局配置",
            Step::UiState => "加载布局与使用状态",
            Step::Plugins => "加载插件",
            Step::OpenRepo => "打开仓库",
            Step::OpenSpace => "打开协作空间",
            Step::SpaceSession => "校验空间会话",
            Step::SpaceConnect => "连接协作空间",
            Step::VaultConfig => "加载仓库配置",
            Step::FileTree => "加载文件树与画布列表",
            Step::AiSessions => "加载 AI 会话",
            Step::RestoreDocs => "恢复上次打开的文件",
            Step::Windows => "还原布局窗口",
        }
    }
}

/// 就绪闸门：全部到达（或超时）才算启动完成。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// 插件随包同步、扫描、启动装载与注册表投影完成。
    Plugins,
    /// 容器已进入且仓库配置读回（失败同样算到达：不阻塞启动）。
    Vault,
    /// 启动恢复的文档全部收尾（打开成功、无法路由或失败）。
    RestoreDocs,
}

fn limit_of(gate: Gate) -> Duration {
    match gate {
        Gate::Plugins => PLUGINS_LIMIT,
        Gate::Vault => VAULT_LIMIT,
        Gate::RestoreDocs => DOCS_LIMIT,
    }
}

impl Gate {
    /// 超时日志用文案。
    pub fn label(self) -> &'static str {
        match self {
            Gate::Plugins => "插件装载",
            Gate::Vault => "仓库配置",
            Gate::RestoreDocs => "文档恢复",
        }
    }
}

struct GateDeadline {
    gate: Gate,
    at: Instant,
}

/// 启动状态：步骤推进、闸门到达与超时放行，以及覆盖层期间的等待面；就绪只发生一次。
pub struct Startup {
    steps: Vec<Step>,
    gates: Vec<GateDeadline>,
    began: bool,
    /// 闸门已全部到达、工作区已在覆盖层下渲染（插件面此刻开始上报真实尺寸）。
    presenting: bool,
    /// 等待面清单已登记（登记只发生一次）。
    faces_registered: bool,
    /// 覆盖层期间仍在等出帧的面（`(插件 id, 贡献 id)`）。
    faces: Vec<(String, String)>,
    finished: bool,
    started: Instant,
    finished_at: Option<Instant>,
    /// 待收尾的恢复文档数与已收尾数。
    docs_expected: usize,
    docs_done: usize,
    sweep: f32,
    spin: f32,
}

impl Startup {
    pub fn new(now: Instant, gates: &[Gate]) -> Self {
        Startup {
            // 全局配置与布局状态在 boot 内同步读入，首帧时已是完成态
            steps: vec![Step::GlobalConfig, Step::UiState],
            gates: gates
                .iter()
                .map(|g| GateDeadline { gate: *g, at: now + limit_of(*g) })
                .collect(),
            began: false,
            presenting: false,
            faces_registered: false,
            faces: Vec::new(),
            finished: false,
            started: now,
            finished_at: None,
            docs_expected: 0,
            docs_done: 0,
            sweep: 0.0,
            spin: 0.0,
        }
    }

    pub fn began(&self) -> bool {
        self.began
    }

    pub fn finished(&self) -> bool {
        self.finished
    }

    /// 工作区已在覆盖层下渲染（就绪后仍为真：主窗视图根不变）。
    pub fn presenting(&self) -> bool {
        self.presenting
    }

    /// 覆盖层是否仍在遮挡工作区。
    pub fn covering(&self) -> bool {
        self.presenting && !self.finished
    }

    pub fn faces_registered(&self) -> bool {
        self.faces_registered
    }

    /// 覆盖层期间仍在等出帧的面。
    pub fn pending_faces(&self) -> &[(String, String)] {
        &self.faces
    }

    pub fn steps(&self) -> &[Step] {
        &self.steps
    }

    pub fn sweep(&self) -> f32 {
        self.sweep
    }

    pub fn spin(&self) -> f32 {
        self.spin
    }

    /// 启动耗时（就绪后固定）。
    pub fn elapsed(&self, now: Instant) -> Duration {
        self.finished_at.unwrap_or(now).saturating_duration_since(self.started)
    }

    /// 标记首帧已渲染、重活开始：只生效一次。
    pub fn begin(&mut self) -> bool {
        if self.began {
            return false;
        }
        self.began = true;
        self.enter(Step::Plugins);
        true
    }

    /// 推进到指定步骤（已出现则忽略，清单只增不改；就绪后不再追加）。
    pub fn enter(&mut self, step: Step) {
        if self.finished {
            return;
        }
        if !self.steps.contains(&step) {
            self.steps.push(step);
        }
    }

    /// 登记待收尾的恢复文档数，文档闸门的兜底上限从此刻起算（读盘发生在进入容器之后）。
    pub fn expect_docs(&mut self, n: usize, now: Instant) {
        self.docs_expected = n;
        self.docs_done = 0;
        if let Some(gate) = self.gates.iter_mut().find(|g| g.gate == Gate::RestoreDocs) {
            gate.at = now + DOCS_LIMIT;
        }
    }

    /// 一份恢复文档收尾：全部收尾即到达文档闸门。
    pub fn docs_done(&mut self, now: Instant) -> bool {
        if self.docs_done < self.docs_expected {
            self.docs_done += 1;
        }
        if self.docs_done >= self.docs_expected {
            self.arrive(Gate::RestoreDocs, now)
        } else {
            false
        }
    }

    /// 闸门到达；返回是否因此就绪。
    pub fn arrive(&mut self, gate: Gate, now: Instant) -> bool {
        self.gates.retain(|g| g.gate != gate);
        self.advance(now)
    }

    /// 超时检查：到点的闸门直接放行，总上限到点则全部放行；返回放行的闸门与该次是否因此就绪。
    pub fn expire(&mut self, now: Instant) -> (Vec<Gate>, bool) {
        let mut expired: Vec<Gate> =
            self.gates.iter().filter(|g| g.at <= now).map(|g| g.gate).collect();
        if now >= self.started + STARTUP_TOTAL {
            expired = self.gates.iter().map(|g| g.gate).collect();
            self.gates.clear();
        } else {
            self.gates.retain(|g| g.at > now);
        }
        (expired, self.advance(now))
    }

    /// 扫光与转圈的相位推进（仅加载屏期间由定时消息驱动）。
    pub fn animate(&mut self) {
        self.sweep = (self.sweep + SWEEP_STEP) % 1.0;
        self.spin = (self.spin + SPIN_STEP) % std::f32::consts::TAU;
    }

    /// 登记覆盖层期间要等的面（闸门未全到或已登记则不生效）；返回是否因此就绪。
    pub fn begin_faces(&mut self, faces: Vec<(String, String)>, now: Instant) -> bool {
        if self.finished || !self.presenting || self.faces_registered {
            return false;
        }
        self.faces = faces;
        self.faces_registered = true;
        self.advance(now)
    }

    /// 某面已按真实尺寸出帧：移出等待项；全部移出即就绪。返回是否因此就绪。
    pub fn mark_face_ready(&mut self, plugin_id: &str, surface_id: &str, now: Instant) -> bool {
        self.faces
            .retain(|(plugin, surface)| !(plugin == plugin_id && surface == surface_id));
        self.advance(now)
    }

    /// 闸门全到即进入覆盖层；等待面登记完毕且全部就绪才撤屏（不设时间上限）。
    fn advance(&mut self, now: Instant) -> bool {
        if self.finished || !self.gates.is_empty() {
            return false;
        }
        self.presenting = true;
        if !self.faces_registered || !self.faces.is_empty() {
            return false;
        }
        // 关闭加载屏前的最后一步：还原布局窗口
        self.enter(Step::Windows);
        self.finished = true;
        self.finished_at = Some(now);
        true
    }
}

/// 加载屏视图：居中应用图标 + 循环扫光条 + 步骤清单（已完成打勾、当前项转圈）。
pub fn view(app: &App) -> Element<'_, Message> {
    let startup = &app.startup;
    let logo = image(app_icon::icon_handle())
        .width(Length::Fixed(LOGO))
        .height(Length::Fixed(LOGO))
        .border_radius(pal::RADIUS_MD);
    let bar = canvas(Sweep { phase: startup.sweep() })
        .width(Length::Fixed(LIST_W))
        .height(Length::Fixed(SWEEP_H));
    let list = container(steps_column(startup))
        .width(Length::Fixed(LIST_W))
        .height(Length::Fixed(LIST_H))
        .align_y(iced::Alignment::Start);
    container(
        column![
            logo,
            space().height(Length::Fixed(22.0)),
            bar,
            space().height(Length::Fixed(26.0)),
            list,
        ]
        .align_x(iced::Alignment::Center),
    )
    .width(Length::Fill)
    .height(Length::Fill)
    .align_x(iced::Alignment::Center)
    .align_y(iced::Alignment::Center)
    .style(|_| container::Style {
        background: Some(pal::BG_PRIMARY.into()),
        ..Default::default()
    })
    .into()
}

/// 主窗视图：覆盖层期间 = 工作区在下、加载屏不透明在上；就绪后只留同根的工作区层。
/// 视图根在覆盖层前后保持 `Stack`，撤屏不重建工作区子树（整面帧的跨帧状态随之保留）。
pub fn main_window_view<'a>(app: &'a App, window: iced::window::Id) -> Element<'a, Message> {
    if !app.startup.presenting() {
        return view(app);
    }
    let mut layers = Stack::new().push(crate::layout_view::main_view(app, window));
    if app.startup.covering() {
        layers = layers.push(cover(app));
    }
    layers.into()
}

/// 不透明覆盖层：铺满窗口并吞掉指针消息，点击 / 滚动落不到下面的工作区。
fn cover(app: &App) -> Element<'_, Message> {
    mouse_area(view(app))
        .on_press(Message::Noop)
        .on_right_press(Message::Noop)
        .on_middle_press(Message::Noop)
        .on_scroll(|_| Message::Noop)
        .into()
}

/// 覆盖层期间需要壳层吞掉的窗口级指针消息（部件层的捕获挡不住全局事件订阅）。
pub fn covering_blocks(message: &Message) -> bool {
    matches!(message, Message::Press(_) | Message::CursorMoved(..) | Message::Release)
}

/// 等待面清单的日志文本（`插件/贡献`，顿号分隔）。
pub fn faces_text(faces: &[(String, String)]) -> String {
    faces.iter().map(|(plugin, surface)| format!("{plugin}/{surface}")).collect::<Vec<_>>().join("、")
}

fn steps_column(startup: &Startup) -> iced::widget::Column<'_, Message> {
    let steps = startup.steps();
    let current = steps.len().saturating_sub(1);
    let mut col = iced::widget::Column::new();
    for (index, step) in steps.iter().enumerate() {
        let done = startup.finished() || index < current;
        let mark = if done {
            ui::icon::icon("check", 12.0, pal::SUCCESS)
        } else {
            ui::icon::icon_spin("loader-circle", 12.0, pal::ACCENT, 1.5, startup.spin())
        };
        let label = ui::text(step.label())
            .size(pal::fs_micro())
            .color(if done { pal::TEXT_MUTED } else { pal::TEXT_PRIMARY });
        col = col.push(
            container(row![mark, label].spacing(8).align_y(iced::Alignment::Center))
                .height(Length::Fixed(ROW_H))
                .align_y(iced::Alignment::Center),
        );
    }
    col
}

/// 扫光条：轨道底色 + 一段循环右移的高亮段（不确定进度）。
struct Sweep {
    phase: f32,
}

impl<Message> canvas::Program<Message> for Sweep {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &iced::Theme,
        bounds: iced::Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let width = bounds.width;
        frame.fill_rectangle(Point::ORIGIN, Size::new(width, bounds.height), pal::BG_TERTIARY);
        let segment = (width * SWEEP_SEG).max(40.0);
        let left = (self.phase * (width + segment) - segment).max(0.0);
        let right = (self.phase * (width + segment)).min(width);
        if right > left {
            frame.fill_rectangle(
                Point::new(left, 0.0),
                Size::new(right - left, bounds.height),
                pal::ACCENT,
            );
        }
        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn startup() -> (Startup, Instant) {
        let now = Instant::now();
        (Startup::new(now, &[Gate::Plugins, Gate::Vault, Gate::RestoreDocs]), now)
    }

    #[test]
    fn boot_steps_are_complete_before_first_frame() {
        let (s, _) = startup();
        assert_eq!(s.steps(), &[Step::GlobalConfig, Step::UiState]);
        assert!(!s.began());
        assert!(!s.finished());
    }

    #[test]
    fn begin_enters_plugins_once() {
        let (mut s, _) = startup();
        assert!(s.begin());
        assert_eq!(s.steps().last(), Some(&Step::Plugins));
        // 重复的首帧消息不得再次推进
        assert!(!s.begin());
        assert_eq!(s.steps().len(), 3);
    }

    #[test]
    fn gates_arriving_only_enters_the_cover_until_faces_report() {
        let (mut s, now) = startup();
        s.begin();
        assert!(!s.arrive(Gate::Plugins, now));
        assert!(!s.arrive(Gate::Vault, now));
        // 闸门全到只进入覆盖层：工作区先渲染，撤屏还得等等待面出帧
        assert!(!s.arrive(Gate::RestoreDocs, now));
        assert!(s.presenting());
        assert!(!s.finished());
        assert_eq!(s.steps().last(), Some(&Step::Plugins));
        assert!(!s.begin_faces(vec![("com.a.canvas".into(), "canvas".into())], now));
        assert!(!s.finished());
        // 面出帧即就绪，且只发生一次
        assert!(s.mark_face_ready("com.a.canvas", "canvas", now));
        assert!(s.finished());
        assert_eq!(s.steps().last(), Some(&Step::Windows));
        assert!(!s.mark_face_ready("com.a.canvas", "canvas", now));
        assert!(!s.arrive(Gate::RestoreDocs, now));
        let (expired, finished) = s.expire(now + Duration::from_secs(60));
        assert!(expired.is_empty());
        assert!(!finished);
    }

    #[test]
    fn empty_face_list_finishes_without_waiting() {
        let (mut s, now) = startup();
        s.begin();
        s.arrive(Gate::Plugins, now);
        s.arrive(Gate::Vault, now);
        assert!(!s.arrive(Gate::RestoreDocs, now));
        assert!(s.begin_faces(Vec::new(), now));
        assert!(s.finished());
    }

    #[test]
    fn faces_cannot_be_registered_before_the_gates_are_clear() {
        let (mut s, now) = startup();
        s.begin();
        s.arrive(Gate::Plugins, now);
        // 仓库闸门还没到：不能提前登记等待面，也不能因此就绪
        assert!(!s.begin_faces(vec![("com.a.canvas".into(), "canvas".into())], now));
        assert!(!s.faces_registered());
        assert!(!s.finished());
    }

    #[test]
    fn a_single_failed_step_does_not_block_the_others() {
        let (mut s, now) = startup();
        s.begin();
        // 仓库失败也按到达处理：后续闸门继续推进
        s.arrive(Gate::Vault, now);
        s.arrive(Gate::RestoreDocs, now);
        assert!(!s.arrive(Gate::Plugins, now));
        assert!(s.presenting());
        assert!(s.begin_faces(Vec::new(), now));
        assert!(s.finished());
    }

    #[test]
    fn deadlines_let_startup_pass_after_a_stuck_gate() {
        let (mut s, now) = startup();
        s.begin();
        s.arrive(Gate::Plugins, now);
        s.arrive(Gate::RestoreDocs, now);
        // 仓库闸门卡住：到点放行
        let (expired, finished) = s.expire(now + VAULT_LIMIT - Duration::from_millis(1));
        assert!(expired.is_empty());
        assert!(!finished);
        let (expired, finished) = s.expire(now + VAULT_LIMIT);
        assert_eq!(expired, vec![Gate::Vault]);
        assert!(!finished);
        assert!(s.begin_faces(Vec::new(), now));
        assert!(s.finished());
    }

    #[test]
    fn restore_docs_gate_waits_for_every_request() {
        let (mut s, now) = startup();
        s.begin();
        s.expect_docs(2, now);
        s.arrive(Gate::Plugins, now);
        s.arrive(Gate::Vault, now);
        assert!(!s.docs_done(now));
        assert!(!s.docs_done(now));
        assert!(s.begin_faces(Vec::new(), now));
        assert!(s.finished());
        // 多报一次收尾不会让计数越过预期，也不会重复就绪
        assert!(!s.docs_done(now));
    }

    #[test]
    fn docs_deadline_counts_from_when_the_restore_requests_are_issued() {
        let (mut s, now) = startup();
        s.begin();
        s.arrive(Gate::Plugins, now);
        s.arrive(Gate::Vault, now);
        // 进入容器较晚：文档闸门的上限从登记时刻起算，而不是从启动起算
        let armed = now + VAULT_LIMIT + Duration::from_millis(200);
        s.expect_docs(1, armed);
        assert!(s.expire(armed - Duration::from_millis(1)).0.is_empty());
        assert_eq!(s.expire(armed + DOCS_LIMIT).0, vec![Gate::RestoreDocs]);
        assert!(s.begin_faces(Vec::new(), now));
        assert!(s.finished());
    }

    #[test]
    fn total_cap_releases_every_gate_at_once() {
        let (mut s, now) = startup();
        s.begin();
        // 各闸门各自的上限都还没到，总上限一到就全部放行
        let (expired, finished) = s.expire(now + STARTUP_TOTAL);
        assert_eq!(expired.len(), 3);
        assert!(!finished);
        assert!(s.begin_faces(Vec::new(), now));
        assert!(s.finished());
    }

    #[test]
    fn cover_swallows_pointer_messages() {
        for blocked in [
            Message::Press(iced::window::Id::unique()),
            Message::CursorMoved(iced::window::Id::unique(), Point::ORIGIN),
            Message::Release,
        ] {
            assert!(covering_blocks(&blocked));
        }
        assert!(!covering_blocks(&Message::Noop));
        assert!(!covering_blocks(&Message::StartupFrameTick));
    }
}
