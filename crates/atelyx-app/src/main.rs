//! Atelyx 原生壳：单主窗口 + 侧栏文件树 + 纯文本编辑保存。
//!
//! 存储与目录过滤全部经 atelyx-core 提供，本文件不含磁盘访问语义。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use atelyx_core::vault;
use iced::widget::{button, column, container, row, scrollable, text, text_editor};
use iced::{keyboard, Element, Length, Subscription, Task, Theme};

fn main() -> iced::Result {
    iced::application(App::default, App::update, App::view)
        .title("Atelyx")
        .theme(|app: &App| app.theme.clone())
        .subscription(App::subscription)
        .run()
}

/// 一条侧栏可见行：由展开集 + 子目录缓存摊平而来。
struct TreeRow {
    rel: String,
    name: String,
    depth: usize,
    is_dir: bool,
    expanded: bool,
}

#[derive(Debug, Clone)]
enum Message {
    PickVault,
    VaultPicked(Result<PathBuf, String>),
    ToggleDir(String),
    /// gen = 发起任务时的会话世代；回调落地前与当前世代比对，过期即丢弃。
    ChildrenLoaded(String, u64, Result<Vec<(String, bool)>, String>),
    SelectFile(String),
    FileLoaded(u64, Result<(String, String), String>),
    Edited(text_editor::Action),
    Save,
    Saved(u64, Result<(), String>),
    ToggleTheme,
}

struct App {
    vault: Arc<vault::VaultState>,
    theme: Theme,
    /// 当前仓库根（打开仓库后与 vault.root() 一致；仅用于展示与文件树标题）。
    vault_root: Option<PathBuf>,
    /// 目录子项缓存（key = 目录相对路径，根为 ""）。懒加载：展开时未命中才读盘。
    children: HashMap<String, Vec<(String, bool)>>,
    expanded: HashSet<String>,
    /// 当前打开文件的仓库相对路径；None = 未打开。
    current_file: Option<String>,
    editor: text_editor::Content,
    dirty: bool,
    status: String,
}

impl Default for App {
    fn default() -> Self {
        Self {
            vault: Arc::new(vault::VaultState::default()),
            theme: Theme::Dark,
            vault_root: None,
            children: HashMap::new(),
            expanded: HashSet::new(),
            current_file: None,
            editor: text_editor::Content::new(),
            dirty: false,
            status: "打开一个仓库开始".into(),
        }
    }
}

impl App {
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::PickVault => {
                if self.dirty {
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
                self.children.clear();
                self.expanded.clear();
                self.current_file = None;
                self.editor = text_editor::Content::new();
                self.dirty = false;
                self.status = "仓库已打开".into();
                self.load_children("")
            }
            Message::VaultPicked(Err(e)) => {
                self.status = e;
                Task::none()
            }
            Message::ToggleDir(rel) => {
                if !self.expanded.remove(&rel) {
                    self.expanded.insert(rel.clone());
                    if !self.children.contains_key(&rel) {
                        return self.load_children(&rel);
                    }
                }
                Task::none()
            }
            Message::ChildrenLoaded(dir, gen, Ok(entries)) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                self.children.insert(dir, entries);
                Task::none()
            }
            Message::ChildrenLoaded(_, _, Err(e)) => {
                self.status = format!("读取目录失败：{e}");
                Task::none()
            }
            Message::SelectFile(rel) => {
                if self.dirty {
                    self.status = "有未保存改动，先 Ctrl+S 保存".into();
                    return Task::none();
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
                    move |result| Message::FileLoaded(gen, result),
                )
            }
            Message::FileLoaded(gen, Ok((rel, text))) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                self.current_file = Some(rel);
                self.editor = text_editor::Content::with_text(&text);
                self.dirty = false;
                self.status = "已打开".into();
                Task::none()
            }
            Message::FileLoaded(_, Err(e)) => {
                self.status = e;
                Task::none()
            }
            Message::Edited(action) => {
                self.editor.perform(action);
                self.dirty = true;
                Task::none()
            }
            Message::Save => {
                let Some(rel) = self.current_file.clone() else {
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
                let content = self.editor.text();
                self.dirty = false;
                Task::perform(
                    async move {
                        let path = vault::safe_join(&root, &rel, false)?;
                        vault::atomic_write(&path, &content)
                    },
                    move |result| Message::Saved(gen, result),
                )
            }
            Message::Saved(gen, Ok(())) => {
                if gen != self.epoch() {
                    return Task::none();
                }
                if !self.dirty {
                    self.status = "已保存".into();
                }
                Task::none()
            }
            Message::Saved(_, Err(e)) => {
                self.dirty = true;
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

    /// 读目录子项（core 的统一过滤语义：隐藏项 / 排除夹 / .tmp 副产物）。
    /// 仓库根与排除列表在发起时冻结，任务在途的仓库切换不影响读盘目标。
    fn load_children(&mut self, rel: &str) -> Task<Message> {
        let Ok(root) = self.vault.root() else {
            self.status = "仓库未打开".into();
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
            move |result| Message::ChildrenLoaded(rel_for_cb, gen, result),
        )
    }

    /// 由子项缓存 + 展开集摊平侧栏行（目录在前、按名升序）。
    fn tree_rows(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        self.push_children(&mut rows, "", 0);
        rows
    }

    /// 递归展开 `dir` 的可见子树（目录在前、忽略大小写按名升序）。
    fn push_children(&self, rows: &mut Vec<TreeRow>, dir: &str, depth: usize) {
        let mut entries: Vec<(String, bool)> = self
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
            let expanded = self.expanded.contains(&rel);
            let name = rel.rsplit('/').next().unwrap_or(&rel).to_string();
            rows.push(TreeRow {
                name,
                depth,
                is_dir,
                expanded,
                rel: rel.clone(),
            });
            if is_dir && expanded {
                self.push_children(rows, &rel, depth + 1);
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let sidebar = if self.vault_root.is_some() {
            let mut tree_col = column![];
            for r in self.tree_rows() {
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
                        Some(Message::ToggleDir(r.rel.clone()))
                    } else {
                        Some(Message::SelectFile(r.rel.clone()))
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

        let editor_area: Element<'_, Message> = if self.current_file.is_some() {
            text_editor(&self.editor)
                .height(Length::Fill)
                .on_action(Message::Edited)
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
            button("切换仓库").on_press_maybe(self.vault_root.is_some().then_some(Message::PickVault)),
            button(if matches!(self.theme, Theme::Dark) { "浅色" } else { "深色" })
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

    fn subscription(&self) -> Subscription<Message> {
        keyboard::listen().filter_map(|event| match event {
            keyboard::Event::KeyPressed {
                key: keyboard::Key::Character(c),
                modifiers,
                ..
            } if c == "s" && modifiers.control() => Some(Message::Save),
            _ => None,
        })
    }
}
