//! Atelyx 原生壳：daemon 多窗口 + 窗口内文件标签 + 侧栏文件树 + 纯文本编辑保存。
//!
//! 存储与目录过滤全部经 atelyx-core 提供，本文件不含磁盘访问语义。
//! 仓库为全局单激活：任一窗口切换仓库全局生效，所有窗口的文件树与标签
//! 随之重置（防止旧仓库的标签向新仓库写盘）；在途任务按仓库世代校验丢弃。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use atelyx_core::vault;
use iced::widget::{button, column, container, row, scrollable, space, text, text_editor};
use iced::window::{self, Settings};
use iced::{keyboard, Element, Event, Length, Size, Subscription, Task, Theme, Vector};

fn main() -> iced::Result {
    iced::daemon(App::boot, App::update, App::view)
        .title(App::title)
        .theme(|app: &App, _id| app.theme.clone())
        .subscription(App::subscription)
        .run()
}

const WINDOW_SIZE: Size = Size::new(1100.0, 760.0);

fn window_settings() -> Settings {
    Settings {
        size: WINDOW_SIZE,
        // 关窗请求统一走 CloseRequested 事件，脏标签在途时可拦截不关
        exit_on_close_request: false,
        ..Settings::default()
    }
}

/// 一条侧栏可见行：由展开集 + 子目录缓存摊平而来。
struct TreeRow {
    rel: String,
    name: String,
    depth: usize,
    is_dir: bool,
    expanded: bool,
}

/// 一个打开的文件标签：编辑内容与脏标记随标签走，切换标签不丢内容。
struct Tab {
    rel: String,
    editor: text_editor::Content,
    dirty: bool,
}

/// 单个窗口的会话状态：文件树与标签归属窗口，仓库状态不在其内。
#[derive(Default)]
struct Window {
    /// 目录子项缓存（key = 目录相对路径，根为 ""）。懒加载：展开时未命中才读盘。
    children: HashMap<String, Vec<(String, bool)>>,
    expanded: HashSet<String>,
    tabs: Vec<Tab>,
    active: Option<usize>,
}

impl Window {
    fn active_tab(&self) -> Option<&Tab> {
        self.active.and_then(|i| self.tabs.get(i))
    }

    fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        let i = self.active?;
        self.tabs.get_mut(i)
    }
}

#[derive(Debug, Clone)]
enum Message {
    NewWindow,
    WindowOpened(window::Id),
    /// 用户点窗口关闭按钮；此处有机会按脏标记拦截。
    WindowCloseRequested(window::Id),
    WindowClosed(window::Id),
    PickVault,
    VaultPicked(Result<PathBuf, String>),
    ToggleDir(window::Id, String),
    /// gen = 发起任务时的仓库世代；回调落地前与当前世代比对，过期即丢弃。
    ChildrenLoaded(window::Id, String, u64, Result<Vec<(String, bool)>, String>),
    SelectFile(window::Id, String),
    FileLoaded(window::Id, u64, Result<(String, String), String>),
    TabSelected(window::Id, usize),
    TabClosed(window::Id, usize),
    Edited(window::Id, text_editor::Action),
    Save(window::Id),
    /// 携带保存发起时冻结的路径与内容：落地后按路径定位标签、与编辑器现值比对，
    /// 区分保存完成与在途新编辑（不取活动标签，写盘在途的标签切换不影响判定）。
    Saved(window::Id, u64, String, String, Result<(), String>),
    ToggleTheme,
}

struct App {
    windows: BTreeMap<window::Id, Window>,
    vault: Arc<vault::VaultState>,
    theme: Theme,
    /// 当前仓库根（打开仓库后与 vault.root() 一致；仅用于展示）。
    vault_root: Option<PathBuf>,
    status: String,
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let (_, open) = window::open(window_settings());
        (
            Self {
                windows: BTreeMap::new(),
                vault: Arc::new(vault::VaultState::default()),
                theme: Theme::Dark,
                vault_root: None,
                status: "打开一个仓库开始".into(),
            },
            open.map(Message::WindowOpened),
        )
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::NewWindow => {
                let Some(last) = self.windows.keys().last().copied() else {
                    return window::open(window_settings()).1.map(Message::WindowOpened);
                };
                window::position(last)
                    .then(|pos| {
                        let mut settings = window_settings();
                        settings.position = pos.map_or(window::Position::Default, |p| {
                            window::Position::Specific(p + Vector::new(24.0, 24.0))
                        });
                        window::open(settings).1
                    })
                    .map(Message::WindowOpened)
            }
            Message::WindowOpened(id) => {
                self.windows.insert(id, Window::default());
                self.load_children(id, "")
            }
            Message::WindowCloseRequested(id) => {
                let dirty = self
                    .windows
                    .get(&id)
                    .is_some_and(|w| w.tabs.iter().any(|t| t.dirty));
                if dirty {
                    self.status = "窗口内有未保存改动，先 Ctrl+S 保存".into();
                    Task::none()
                } else {
                    window::close(id)
                }
            }
            Message::WindowClosed(id) => {
                self.windows.remove(&id);
                if self.windows.is_empty() {
                    iced::exit()
                } else {
                    Task::none()
                }
            }
            Message::PickVault => {
                if self.any_dirty() {
                    self.status = "有未保存改动，先 Ctrl+S 保存".into();
                    return Task::none();
                }
                Task::perform(
                    async move {
                        match rfd::AsyncFileDialog::new().pick_folder().await {
                            Some(handle) => Ok(handle.path().to_path_buf()),
                            None => Err("未选择文件夹".into()),
                        }
                    },
                    Message::VaultPicked,
                )
            }
            Message::VaultPicked(Ok(path)) => {
                if let Err(e) = self.vault.set(path.clone(), Vec::new()) {
                    self.status = e;
                    return Task::none();
                }
                self.vault_root = self.vault.root().ok();
                self.status = "仓库已打开".into();
                // 所有窗口的树与标签都指向旧仓库，全部重置防跨仓库读写
                let ids: Vec<window::Id> = self.windows.keys().copied().collect();
                let mut task = Task::none();
                for id in ids {
                    if let Some(w) = self.windows.get_mut(&id) {
                        w.children.clear();
                        w.expanded.clear();
                        w.tabs.clear();
                        w.active = None;
                    }
                    task = task.chain(self.load_children(id, ""));
                }
                task
            }
            Message::VaultPicked(Err(e)) => {
                self.status = e;
                Task::none()
            }
            Message::ToggleDir(id, rel) => {
                let need_load = if let Some(w) = self.windows.get_mut(&id) {
                    if !w.expanded.remove(&rel) {
                        w.expanded.insert(rel.clone());
                        !w.children.contains_key(&rel)
                    } else {
                        false
                    }
                } else {
                    false
                };
                if need_load {
                    return self.load_children(id, &rel);
                }
                Task::none()
            }
            Message::ChildrenLoaded(id, dir, gen, Ok(entries)) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                if let Some(w) = self.windows.get_mut(&id) {
                    w.children.insert(dir, entries);
                }
                Task::none()
            }
            Message::ChildrenLoaded(_, _, _, Err(e)) => {
                self.status = format!("读取目录失败：{e}");
                Task::none()
            }
            Message::SelectFile(id, rel) => {
                // 已开标签直接激活；否则读盘开新标签
                if let Some(w) = self.windows.get_mut(&id) {
                    if let Some(i) = w.tabs.iter().position(|t| t.rel == rel) {
                        w.active = Some(i);
                        return Task::none();
                    }
                }
                // 仓库根在发起时冻结：任务在途时切换仓库不会让读盘落到新仓库路径。
                let Ok(root) = self.vault.root() else {
                    self.status = "仓库未打开".into();
                    return Task::none();
                };
                let gen = self.epoch();
                Task::perform(
                    async move {
                        let path = vault::safe_join(&root, &rel, false)?;
                        let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                        let text = String::from_utf8(bytes)
                            .map_err(|_| "非 UTF-8 文本文件，暂不支持打开".to_string())?;
                        Ok((rel, text))
                    },
                    move |result| Message::FileLoaded(id, gen, result),
                )
            }
            Message::FileLoaded(id, gen, Ok((rel, text))) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                if let Some(w) = self.windows.get_mut(&id) {
                    if let Some(i) = w.tabs.iter().position(|t| t.rel == rel) {
                        w.active = Some(i);
                    } else {
                        w.tabs.push(Tab {
                            rel,
                            editor: text_editor::Content::with_text(&text),
                            dirty: false,
                        });
                        w.active = Some(w.tabs.len() - 1);
                    }
                    self.status = "已打开".into();
                }
                Task::none()
            }
            Message::FileLoaded(_, _, Err(e)) => {
                self.status = e;
                Task::none()
            }
            Message::TabSelected(id, i) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    if i < w.tabs.len() {
                        w.active = Some(i);
                    }
                }
                Task::none()
            }
            Message::TabClosed(id, idx) => {
                let dirty = self
                    .windows
                    .get(&id)
                    .and_then(|w| w.tabs.get(idx))
                    .is_some_and(|t| t.dirty);
                if dirty {
                    self.status = "有未保存改动，先 Ctrl+S 保存".into();
                    return Task::none();
                }
                let Some(w) = self.windows.get_mut(&id) else {
                    return Task::none();
                };
                if idx >= w.tabs.len() {
                    return Task::none();
                }
                w.tabs.remove(idx);
                w.active = match w.active {
                    Some(a) if a > idx => Some(a - 1),
                    Some(a) if a >= w.tabs.len() => w.tabs.len().checked_sub(1),
                    a => a,
                };
                Task::none()
            }
            Message::Edited(id, action) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    if let Some(tab) = w.active_tab_mut() {
                        // 动作须全部执行（widget 只发布不应用，光标/滚动依赖此处）；
                        // 置脏只看内容是否实际变化——编辑动作也可能是空操作
                        // （空缓冲退格等），光标/选区/滚动类直接跳过比对
                        let may_edit = action.is_edit();
                        let before = may_edit.then(|| tab.editor.text());
                        tab.editor.perform(action);
                        if let Some(before) = before {
                            if tab.editor.text() != before {
                                tab.dirty = true;
                            }
                        }
                    }
                }
                Task::none()
            }
            Message::Save(id) => {
                let Some(w) = self.windows.get(&id) else {
                    return Task::none();
                };
                let Some(tab) = w.active_tab() else {
                    self.status = "没有打开的文件".into();
                    return Task::none();
                };
                // 仓库根在发起时冻结：写盘目标固定为打开该文件时的仓库，
                // 不随在途期间的仓库切换漂移（防跨仓库写入）。
                let Ok(root) = self.vault.root() else {
                    self.status = "仓库未打开".into();
                    return Task::none();
                };
                let gen = self.epoch();
                let rel = tab.rel.clone();
                let saved_rel = rel.clone();
                let content = tab.editor.text();
                let saved = content.clone();
                Task::perform(
                    async move {
                        let path = vault::safe_join(&root, &rel, false)?;
                        vault::atomic_write(&path, &content)
                    },
                    move |result| Message::Saved(id, gen, saved_rel, saved, result),
                )
            }
            Message::Saved(id, gen, rel, saved, Ok(())) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                // 保存发起时冻结的内容与当前一致才算保存完成；
                // 在途期间的新编辑保持脏标记
                if let Some(tab) = self
                    .windows
                    .get_mut(&id)
                    .and_then(|w| w.tabs.iter_mut().find(|t| t.rel == rel))
                {
                    if tab.editor.text() == saved {
                        tab.dirty = false;
                        self.status = "已保存".into();
                    } else {
                        self.status = "保存完成，期间有新修改".into();
                    }
                }
                Task::none()
            }
            Message::Saved(_, _, _, _, Err(e)) => {
                self.status = format!("保存失败：{e}");
                Task::none()
            }
            Message::ToggleTheme => {
                self.theme = match self.theme {
                    Theme::Dark => Theme::Light,
                    _ => Theme::Dark,
                };
                Task::none()
            }
        }
    }

    /// 会话世代：core 在每次切换仓库时自增；在途任务的回调据此判定是否过期。
    fn epoch(&self) -> u64 {
        self.vault
            .generation
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// 任一窗口存在脏标签即视为全局有未保存改动（切仓库前守卫）。
    fn any_dirty(&self) -> bool {
        self.windows
            .values()
            .any(|w| w.tabs.iter().any(|t| t.dirty))
    }

    /// 读目录子项（core 的统一过滤语义：隐藏项 / 排除夹 / .tmp 副产物）。
    /// 仓库根与排除列表在发起时冻结，任务在途的仓库切换不影响读盘目标。
    fn load_children(&mut self, id: window::Id, rel: &str) -> Task<Message> {
        let Ok(root) = self.vault.root() else {
            return Task::none();
        };
        let Ok(exclude) = self.vault.exclude_folders() else {
            self.status = "仓库配置读取失败".into();
            return Task::none();
        };
        let gen = self.epoch();
        let rel = rel.to_string();
        let rel_for_cb = rel.clone();
        Task::perform(
            async move { vault::read_dir_filtered(&root, &rel, &exclude) },
            move |result| Message::ChildrenLoaded(id, rel_for_cb, gen, result),
        )
    }

    fn title(&self, id: window::Id) -> String {
        self.windows
            .get(&id)
            .and_then(|w| w.active_tab())
            .map(|t| {
                let name = t.rel.rsplit('/').next().unwrap_or(&t.rel);
                format!("Atelyx — {name}")
            })
            .unwrap_or_else(|| "Atelyx".into())
    }

    fn view(&self, id: window::Id) -> Element<'_, Message> {
        // 窗口登记（WindowOpened）前的首帧
        let Some(w) = self.windows.get(&id) else {
            return space().into();
        };

        let sidebar = if self.vault_root.is_some() {
            let mut tree_col = column![];
            for r in Self::tree_rows(w) {
                let indent = iced::Padding {
                    left: 8.0 + r.depth as f32 * 16.0,
                    ..Default::default()
                };
                let label = if r.is_dir {
                    format!("{} {}", if r.expanded { "▾" } else { "▸" }, r.name)
                } else {
                    format!("  {0}", r.name)
                };
                let row = button(text(label).size(13).width(Length::Fill))
                    .padding(indent)
                    .style(button::text)
                    .on_press_maybe(if r.is_dir {
                        Some(Message::ToggleDir(id, r.rel.clone()))
                    } else {
                        Some(Message::SelectFile(id, r.rel.clone()))
                    })
                    .width(Length::Fill);
                tree_col = tree_col.push(row);
            }
            scrollable(container(tree_col).width(Length::Fill)).width(Length::Fixed(280.0))
        } else {
            scrollable(
                button("打开仓库…")
                    .on_press(Message::PickVault)
                    .padding(8),
            )
            .width(Length::Fixed(280.0))
        };

        let mut tab_row = row![].spacing(4);
        for (i, tab) in w.tabs.iter().enumerate() {
            let name = tab.rel.rsplit('/').next().unwrap_or(&tab.rel).to_string();
            let label = if tab.dirty { format!("{name} •") } else { name };
            let is_active = w.active == Some(i);
            tab_row = tab_row
                .push(
                    button(text(label).size(12))
                        .padding([4, 8])
                        .style(if is_active {
                            button::primary
                        } else {
                            button::text
                        })
                        .on_press_maybe((!is_active).then_some(Message::TabSelected(id, i))),
                )
                .push(
                    button(text("×").size(12))
                        .padding([4, 6])
                        .style(button::text)
                        .on_press(Message::TabClosed(id, i)),
                );
        }

        let editor_area: Element<'_, Message> = if let Some(tab) = w.active_tab() {
            column![
                tab_row,
                text_editor(&tab.editor)
                    .height(Length::Fill)
                    .on_action(move |action| Message::Edited(id, action)),
            ]
            .into()
        } else {
            container(text("左侧选择文件打开").size(14))
                .width(Length::Fill)
                .height(Length::Fill)
                .center_x(Length::Fill)
                .center_y(Length::Fill)
                .into()
        };

        let header = row![
            button("新开窗口").on_press(Message::NewWindow),
            button("切换仓库")
                .on_press_maybe(self.vault_root.is_some().then_some(Message::PickVault)),
            button(if matches!(self.theme, Theme::Dark) {
                "浅色"
            } else {
                "深色"
            })
            .on_press(Message::ToggleTheme),
            text(self.status.clone()).size(12),
        ]
        .spacing(8)
        .padding(8);

        let content = column![
            header,
            row![
                container(sidebar).height(Length::Fill),
                container(editor_area)
                    .width(Length::Fill)
                    .height(Length::Fill)
                    .padding([0, 8]),
            ]
            .height(Length::Fill)
        ]
        .height(Length::Fill);

        content.into()
    }

    /// 由子项缓存 + 展开集摊平侧栏行（目录在前、按名升序）。
    fn tree_rows(w: &Window) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        Self::push_children(&mut rows, w, "", 0);
        rows
    }

    /// 递归展开 `dir` 的可见子树（目录在前、忽略大小写按名升序）。
    fn push_children(rows: &mut Vec<TreeRow>, w: &Window, dir: &str, depth: usize) {
        let mut entries: Vec<(String, bool)> = w
            .children
            .get(dir)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        entries.sort_by(|a, b| {
            b.1.cmp(&a.1).then_with(|| a.0.to_lowercase().cmp(&b.0.to_lowercase()))
        });
        for (rel, is_dir) in entries {
            let expanded = w.expanded.contains(&rel);
            let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
            rows.push(TreeRow {
                name,
                depth,
                is_dir,
                expanded,
                rel: rel.clone(),
            });
            if is_dir && expanded {
                Self::push_children(rows, w, &rel, depth + 1);
            }
        }
    }

    /// 快捷键按来源窗口路由（Ctrl+S 存当前焦点窗口的活动标签）；
    /// 关窗请求与关窗完成也在此统一接入。
    fn subscription(&self) -> Subscription<Message> {
        iced::event::listen_with(|event, _status, window| match event {
            Event::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Character(c),
                modifiers,
                ..
            }) if c == "s" && modifiers.control() => Some(Message::Save(window)),
            Event::Window(window::Event::CloseRequested) => {
                Some(Message::WindowCloseRequested(window))
            }
            Event::Window(window::Event::Closed) => Some(Message::WindowClosed(window)),
            _ => None,
        })
    }
}
