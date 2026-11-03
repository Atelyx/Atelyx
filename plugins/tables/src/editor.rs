//! 文本编辑状态机：光标 / 选区 / 输入法组合串 / 按键语义。单元格编辑与浮层输入共用这一份，
//! 值解析与提交语义由调用方决定（单元格按字段类型解析，浮层按用途取文本）。

use atelyx_plugin_protocol::frame::{Key, Modifiers, NamedKey};

/// 一次按键的处理结果。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyOutcome {
    /// 文本或光标有变化（需要重绘）。
    pub edited: bool,
    /// 请求提交（回车）。
    pub commit: bool,
    /// 请求取消（Esc）。
    pub cancel: bool,
}

/// 文本编辑状态。
#[derive(Clone, Debug, Default)]
pub struct Editor {
    pub text: String,
    pub caret: usize,
    pub anchor: usize,
    /// 输入法组合串与插入点（字节位）。
    pub preedit: String,
    pub preedit_at: usize,
    /// 是否输入过（区分「没动过」与「输入后删空」）。
    pub touched: bool,
}

impl Editor {
    /// 以给定文本起稿，光标与锚点置于末尾。
    pub fn new(text: String) -> Self {
        let caret = text.len();
        Self { text, caret, anchor: caret, ..Self::default() }
    }

    /// 显示用文本（含组合串）。
    pub fn display_text(&self) -> String {
        if self.preedit.is_empty() {
            return self.text.clone();
        }
        let mut text = self.text.clone();
        text.insert_str(self.preedit_at.min(text.len()), &self.preedit);
        text
    }

    /// 显示用光标与锚点（已按组合串长度偏移）。
    pub fn display_caret(&self) -> (usize, usize) {
        let shift = |index: usize| {
            if self.preedit.is_empty() {
                index
            } else if index >= self.preedit_at {
                index + self.preedit.len()
            } else {
                index
            }
        };
        (shift(self.caret), shift(self.anchor))
    }

    /// 选区（有序两端）。
    pub fn selection(&self) -> (usize, usize) {
        ordered(self.caret, self.anchor)
    }

    /// 在光标处插入文本，光标与锚点跟随。
    pub fn insert(&mut self, text: &str) {
        self.text.insert_str(self.caret, text);
        self.caret += text.len();
        self.anchor = self.caret;
    }

    /// 用 `with` 替换 `[from, to)`。
    fn replace(&mut self, from: usize, to: usize, with: &str) {
        self.text.replace_range(from..to, with);
        self.caret = from + with.len();
        self.anchor = self.caret;
    }

    /// 删除选区（无选区不做事）；返回是否有变化。
    fn delete_selection(&mut self) -> bool {
        if self.caret == self.anchor {
            return false;
        }
        let (from, to) = self.selection();
        self.replace(from, to, "");
        true
    }

    /// 退格。
    fn backspace(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.caret == 0 {
            return false;
        }
        let prev = prev_boundary(&self.text, self.caret);
        self.replace(prev, self.caret, "");
        true
    }

    /// 删除。
    fn delete(&mut self) -> bool {
        if self.delete_selection() {
            return true;
        }
        if self.caret >= self.text.len() {
            return false;
        }
        let next = next_boundary(&self.text, self.caret);
        self.replace(self.caret, next, "");
        true
    }

    /// 方向键 / Home / End：`shift` = 扩选。
    fn move_horizontal(&mut self, delta: i32, shift: bool) -> bool {
        let anchor = if shift { self.anchor } else { self.caret };
        let caret = if delta < 0 {
            if self.caret > 0 {
                prev_boundary(&self.text, self.caret)
            } else {
                0
            }
        } else if self.caret < self.text.len() {
            next_boundary(&self.text, self.caret)
        } else {
            self.text.len()
        };
        self.caret = caret;
        self.anchor = if shift { anchor } else { caret };
        true
    }

    /// 处理一次按键（编辑态语义：回车提交、Esc 取消、Shift+Enter 由 `multiline` 决定换行）。
    pub fn apply_key(&mut self, key: &Key, modifiers: &Modifiers, multiline: bool) -> KeyOutcome {
        let mut outcome = KeyOutcome::default();
        match key {
            Key::Named { name } => match name {
                NamedKey::Escape => outcome.cancel = true,
                NamedKey::Enter => {
                    if multiline && modifiers.shift {
                        self.insert("\n");
                        self.touched = true;
                        outcome.edited = true;
                    } else {
                        outcome.commit = true;
                    }
                }
                NamedKey::Backspace => {
                    if self.backspace() {
                        self.touched = true;
                        outcome.edited = true;
                    }
                }
                NamedKey::Delete => {
                    if self.delete() {
                        self.touched = true;
                        outcome.edited = true;
                    }
                }
                NamedKey::Space => {
                    self.delete_selection();
                    self.insert(" ");
                    self.touched = true;
                    outcome.edited = true;
                }
                NamedKey::ArrowLeft => outcome.edited = self.move_horizontal(-1, modifiers.shift),
                NamedKey::ArrowRight => outcome.edited = self.move_horizontal(1, modifiers.shift),
                NamedKey::Home => {
                    self.caret = 0;
                    if !modifiers.shift {
                        self.anchor = 0;
                    }
                    outcome.edited = true;
                }
                NamedKey::End => {
                    self.caret = self.text.len();
                    if !modifiers.shift {
                        self.anchor = self.caret;
                    }
                    outcome.edited = true;
                }
                NamedKey::ArrowUp | NamedKey::ArrowDown => {
                    if multiline {
                        outcome.edited = self.move_vertical(*name == NamedKey::ArrowUp, modifiers.shift);
                    }
                }
                _ => {}
            },
            Key::Character { text } => {
                let control = modifiers.control || modifiers.meta;
                if control {
                    // 输入面的复制 / 粘贴 / 全选由调用方接宿主剪贴板；这里只处理全选
                    if text.eq_ignore_ascii_case("a") {
                        self.anchor = 0;
                        self.caret = self.text.len();
                        outcome.edited = true;
                    }
                    return outcome;
                }
                self.delete_selection();
                self.insert(text);
                self.touched = true;
                outcome.edited = true;
            }
        }
        outcome
    }

    /// 多行文本的上下移动（按行首列近似）。
    fn move_vertical(&mut self, up: bool, shift: bool) -> bool {
        let anchor = if shift { self.anchor } else { self.caret };
        let start = self.text[..self.caret.min(self.text.len())]
            .rfind('\n')
            .map(|index| index + 1)
            .unwrap_or(0);
        let column = self.caret - start;
        let target = if up {
            if start == 0 {
                return false;
            }
            let prev_end = start - 1;
            let prev_start = self.text[..prev_end].rfind('\n').map(|index| index + 1).unwrap_or(0);
            (prev_start + column).min(prev_end)
        } else {
            let line_end = self.text[self.caret.min(self.text.len())..]
                .find('\n')
                .map(|index| self.caret + index)
                .unwrap_or(self.text.len());
            if line_end >= self.text.len() {
                return false;
            }
            let next_start = line_end + 1;
            let next_end = self.text[next_start..]
                .find('\n')
                .map(|index| next_start + index)
                .unwrap_or(self.text.len());
            (next_start + column).min(next_end)
        };
        let target = floor_boundary(&self.text, target);
        self.caret = target;
        self.anchor = if shift { anchor } else { target };
        true
    }

    /// 输入法：组合串更新 / 提交文本。
    pub fn apply_ime(&mut self, text: &str, composing: bool, _caret: Option<u32>) -> bool {
        if composing {
            self.preedit = text.to_string();
            // 组合串从文档光标处开始；组合期间的 caret 不动，不用宿主给的 payload 游标
            self.preedit_at = self.caret;
            return true;
        }
        self.preedit.clear();
        if !text.is_empty() {
            self.delete_selection();
            self.insert(text);
            self.touched = true;
            return true;
        }
        false
    }

    /// 粘贴（替换选区）。
    pub fn paste(&mut self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        self.delete_selection();
        self.insert(text);
        self.touched = true;
        true
    }

    /// 选区文本（无选区 = 空串）。
    pub fn selected_text(&self) -> String {
        let (from, to) = self.selection();
        self.text[from..to].to_string()
    }

    /// 删除选区（无选区不做事）。
    pub fn delete_selection_now(&mut self) -> bool {
        self.delete_selection()
    }

}

/// 字节位向下取整到字符边界：多行文本按字节列对齐时不能落进多字节字符中间。
fn floor_boundary(text: &str, index: usize) -> usize {
    let mut index = index.min(text.len());
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// 前一字符边界。
pub fn prev_boundary(text: &str, index: usize) -> usize {
    text[..index].char_indices().last().map(|(i, _)| i).unwrap_or(0)
}

/// 后一字符边界。
pub fn next_boundary(text: &str, index: usize) -> usize {
    text[index.min(text.len())..]
        .char_indices()
        .nth(1)
        .map(|(i, _)| index + i)
        .unwrap_or_else(|| text.len())
}

/// 两端有序。
pub fn ordered(a: usize, b: usize) -> (usize, usize) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}
