//! 命令注册表：可重映射命令的键位解析、事件匹配、录制与冲突检测。
//! 用户覆盖存 global.json `commandShortcuts`，命中按默认表顺序先到先得。

use std::collections::HashMap;

use iced::keyboard::{self, key::Named};

/// 命令作用域（决定分发时的上下文门控）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scope {
    /// 笔记编辑面（undo/redo 经编辑面绑定表消费）。
    Note,
    /// 表格画布。
    Table,
}

/// 键位规则（录制校验）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rule {
    /// 必须带 Ctrl/Cmd。
    RequireMod,
    /// 不得带 Ctrl/Cmd/Alt（Shift 允许）。
    NoMod,
}

/// 可重映射命令（现役 note + table 两域；id 持久化为 `note.undo` 形态）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommandId {
    NoteUndo,
    NoteRedo,
    TableUndo,
    TableRedo,
    TableCopy,
    TableCut,
    TablePaste,
    TableClear,
}

impl CommandId {
    /// 全部命令（默认表顺序 = 命中优先序；快捷键设置页按此顺序按作用域分组展示）。
    pub const ALL: &'static [CommandId] = &[
        CommandId::NoteUndo,
        CommandId::NoteRedo,
        CommandId::TableUndo,
        CommandId::TableRedo,
        CommandId::TableCopy,
        CommandId::TableCut,
        CommandId::TablePaste,
        CommandId::TableClear,
    ];

    /// 持久化 id（global.json 键）。
    pub fn key(self) -> &'static str {
        match self {
            CommandId::NoteUndo => "note.undo",
            CommandId::NoteRedo => "note.redo",
            CommandId::TableUndo => "table.undo",
            CommandId::TableRedo => "table.redo",
            CommandId::TableCopy => "table.copy",
            CommandId::TableCut => "table.cut",
            CommandId::TablePaste => "table.paste",
            CommandId::TableClear => "table.clear",
        }
    }

    pub fn from_key(key: &str) -> Option<CommandId> {
        CommandId::ALL.iter().copied().find(|c| c.key() == key)
    }

    pub fn scope(self) -> Scope {
        match self {
            CommandId::NoteUndo | CommandId::NoteRedo => Scope::Note,
            _ => Scope::Table,
        }
    }

    pub fn rule(self) -> Rule {
        match self {
            CommandId::TableClear => Rule::NoMod,
            _ => Rule::RequireMod,
        }
    }

    pub fn default_binding(self) -> &'static str {
        match self {
            CommandId::NoteUndo | CommandId::TableUndo => "mod+z",
            CommandId::NoteRedo | CommandId::TableRedo => "mod+y",
            CommandId::TableCopy => "mod+c",
            CommandId::TableCut => "mod+x",
            CommandId::TablePaste => "mod+v",
            CommandId::TableClear => "delete",
        }
    }

    /// 展示名（快捷键设置行；归属由分组标题承载，不含领域前缀）。
    pub fn label(self) -> &'static str {
        match self {
            CommandId::NoteUndo | CommandId::TableUndo => "撤销",
            CommandId::NoteRedo | CommandId::TableRedo => "重做",
            CommandId::TableCopy => "复制",
            CommandId::TableCut => "剪切",
            CommandId::TablePaste => "粘贴",
            CommandId::TableClear => "清空选区",
        }
    }
}

impl Scope {
    /// 快捷键设置页的分组标题。
    pub fn label(self) -> &'static str {
        match self {
            Scope::Note => "笔记",
            Scope::Table => "表格",
        }
    }
}

/// 解析后的键位（修饰集合 + 主键）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Binding {
    pub mod_key: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: String,
}

/// 解析键串 `mod+k`（mod = Ctrl/Cmd 任一，主键小写；非法片段（空段/未知修饰）返回 None）。
/// 修饰集合精确匹配：注册 `mod+k` 不命中 Ctrl+Shift+K。
pub fn parse(s: &str) -> Option<Binding> {
    let mut b = Binding { mod_key: false, alt: false, shift: false, key: String::new() };
    for part in s.split('+') {
        let part = part.trim().to_lowercase();
        match part.as_str() {
            "mod" => b.mod_key = true,
            "ctrl" => b.mod_key = true,
            "cmd" => b.mod_key = true,
            "alt" => b.alt = true,
            "shift" => b.shift = true,
            "" => return None,
            k => {
                if !b.key.is_empty() {
                    return None;
                }
                b.key = k.to_string();
            }
        }
    }
    if b.key.is_empty() {
        None
    } else {
        Some(b)
    }
}

/// 规范序列化（冲突比对与落盘均用此形态：修饰序固定 mod/alt/shift + 主键小写）。
pub fn serialize(b: &Binding) -> String {
    let mut out = String::new();
    if b.mod_key {
        out.push_str("mod+");
    }
    if b.alt {
        out.push_str("alt+");
    }
    if b.shift {
        out.push_str("shift+");
    }
    out.push_str(&b.key);
    out
}

/// 合并默认表与用户覆盖（覆盖仅替换键位，顺序保持默认表序；无效覆盖忽略）。
pub fn resolve(overrides: &HashMap<String, String>) -> Vec<(CommandId, String)> {
    CommandId::ALL
        .iter()
        .map(|c| {
            let binding = overrides
                .get(c.key())
                .filter(|s| parse(s).is_some() && validate(*c, s).is_ok())
                .cloned()
                .unwrap_or_else(|| c.default_binding().to_string());
            (*c, binding)
        })
        .collect()
}

/// 键盘事件键 → 键串主键文本（字符键 = 小写字符；命名键 = 形如 "escape"）。
pub fn key_text(key: &keyboard::Key) -> String {
    match key {
        keyboard::Key::Character(c) => c.to_lowercase(),
        keyboard::Key::Named(n) => named_text(*n).to_string(),
        keyboard::Key::Unidentified => String::new(),
    }
}

fn named_text(n: Named) -> &'static str {
    match n {
        Named::Escape => "escape",
        Named::Enter => "enter",
        Named::Tab => "tab",
        Named::Backspace => "backspace",
        Named::Delete => "delete",
        Named::Home => "home",
        Named::End => "end",
        Named::PageUp => "pageup",
        Named::PageDown => "pagedown",
        Named::ArrowUp => "arrowup",
        Named::ArrowDown => "arrowdown",
        Named::ArrowLeft => "arrowleft",
        Named::ArrowRight => "arrowright",
        Named::Space => "space",
        _ => "other",
    }
}

/// 事件是否命中键串（修饰集合精确匹配：Ctrl/Meta 任一即 mod；事件不得带键串之外的修饰）。
pub fn matches(binding: &str, mods: keyboard::Modifiers, key: &keyboard::Key) -> bool {
    let Some(b) = parse(binding) else { return false };
    let text = key_text(key);
    if b.key != text {
        return false;
    }
    let event_mod = mods.control() || mods.logo();
    b.mod_key == event_mod && b.alt == mods.alt() && b.shift == mods.shift()
}

/// 录制一组合键为键串；纯修饰键 / 未识别键返回 None（调用方继续等待或取消）。
pub fn capture(mods: keyboard::Modifiers, key: &keyboard::Key) -> Option<String> {
    let text = key_text(key);
    match key {
        keyboard::Key::Character(c) if !text.is_empty() => {
            let b = Binding {
                mod_key: mods.control() || mods.logo(),
                alt: mods.alt(),
                shift: mods.shift(),
                // 记录 shift 修饰后交付的字符（如 Shift+1 记 `!`，展示 `shift+!`）；键串大小写不敏感
                key: c.to_lowercase(),
            };
            Some(serialize(&b))
        }
        keyboard::Key::Named(n) if !matches!(named_text(*n), "other") => {
            let b = Binding {
                mod_key: mods.control() || mods.logo(),
                alt: mods.alt(),
                shift: mods.shift(),
                key: named_text(*n).to_string(),
            };
            Some(serialize(&b))
        }
        _ => None,
    }
}

/// 录制校验；Err = 违规原因（用户可见文案）。
pub fn validate(id: CommandId, binding: &str) -> Result<(), &'static str> {
    let Some(b) = parse(binding) else { return Err("无法识别的键位") };
    match id.rule() {
        Rule::RequireMod => {
            if !b.mod_key {
                Err("此命令需包含 Ctrl/Cmd")
            } else {
                Ok(())
            }
        }
        Rule::NoMod => {
            if b.mod_key || b.alt {
                Err("此命令不得包含 Ctrl/Cmd/Alt")
            } else {
                Ok(())
            }
        }
    }
}

/// 同作用域内与 `id` 键串相同的其他命令（允许共存，命中按序先到；用于警示展示）。
pub fn conflicts(resolved: &[(CommandId, String)], id: CommandId) -> Vec<CommandId> {
    let Some((_, binding)) = resolved.iter().find(|(c, _)| *c == id) else { return vec![] };
    resolved
        .iter()
        .filter(|(c, s)| *c != id && c.scope() == id.scope() && s == binding)
        .map(|(c, _)| *c)
        .collect()
}

/// 编辑面部件注入的撤销/重做键位（仅 Note 域两条；部件按键匹配查此表）。
#[derive(Clone, Debug)]
pub struct NoteBindings {
    pub undo: String,
    pub redo: String,
    /// mod+shift+z 重做别名（redo 未被重定义时沿用；部件内匹配）。
    pub redo_alias: bool,
}

impl Default for NoteBindings {
    fn default() -> Self {
        Self {
            undo: CommandId::NoteUndo.default_binding().to_string(),
            redo: CommandId::NoteRedo.default_binding().to_string(),
            redo_alias: true,
        }
    }
}

/// 解析编辑面键位：与 `resolve` 同口径（无效覆盖回退默认表）。
pub fn note_bindings(overrides: &HashMap<String, String>) -> NoteBindings {
    let mut b = NoteBindings::default();
    for (id, binding) in resolve(overrides) {
        match id {
            CommandId::NoteUndo => b.undo = binding,
            CommandId::NoteRedo => b.redo = binding,
            _ => {}
        }
    }
    b.redo_alias = redo_alias_active(overrides, Scope::Note);
    b
}

/// `mod+shift+z` 重做别名是否生效：redo 未被覆盖，且 undo 未被改绑到该键串（避免一键双义）。
pub fn redo_alias_active(overrides: &HashMap<String, String>, scope: Scope) -> bool {
    let (undo_key, redo_key) = match scope {
        Scope::Note => (CommandId::NoteUndo.key(), CommandId::NoteRedo.key()),
        Scope::Table => (CommandId::TableUndo.key(), CommandId::TableRedo.key()),
    };
    !overrides.contains_key(redo_key)
        && overrides
            .get(undo_key)
            // parse+serialize 归一后比较：用户覆盖串的大小写 / 修饰顺序差异不误判
            .map(|s| parse(s).map(|b| serialize(&b)) != Some("mod+shift+z".to_string()))
            .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, alt: bool, shift: bool) -> keyboard::Modifiers {
        let mut m = keyboard::Modifiers::empty();
        if ctrl {
            m |= keyboard::Modifiers::CTRL;
        }
        if alt {
            m |= keyboard::Modifiers::ALT;
        }
        if shift {
            m |= keyboard::Modifiers::SHIFT;
        }
        m
    }

    fn char_key(c: &str) -> keyboard::Key {
        keyboard::Key::Character(c.into())
    }

    #[test]
    fn parse_and_serialize_roundtrip() {
        let b = parse("mod+z").unwrap();
        assert_eq!(serialize(&b), "mod+z");
        // 修饰序与大小写不敏感
        let b = parse("Shift+MOD+K").unwrap();
        assert_eq!(serialize(&b), "mod+shift+k");
        assert!(parse("mod+").is_none());
        assert!(parse("+z").is_none());
        assert!(parse("mod+a+b").is_none());
    }

    #[test]
    fn match_requires_exact_modifier_set() {
        assert!(matches("mod+z", mods(true, false, false), &char_key("z")));
        // meta 也算 mod
        let mut meta = mods(false, false, false);
        meta |= keyboard::Modifiers::LOGO;
        assert!(matches("mod+z", meta, &char_key("z")));
        // 额外 shift 不命中（修饰集合精确匹配）
        assert!(!matches("mod+z", mods(true, false, true), &char_key("z")));
        assert!(!matches("mod+z", mods(false, false, false), &char_key("z")));
        // 主键大小写不敏感
        assert!(matches("mod+a", mods(true, false, false), &char_key("A")));
    }

    #[test]
    fn capture_serializes_event() {
        assert_eq!(capture(mods(true, false, false), &char_key("Z")).as_deref(), Some("mod+z"));
        assert_eq!(capture(mods(false, false, true), &char_key("1")).as_deref(), Some("shift+1"));
        // 纯修饰键（key_text 为空路径外的 Modifier）不产出
        assert_eq!(capture(mods(true, false, false), &keyboard::Key::Named(Named::Shift)), None);
        assert_eq!(capture(mods(false, false, false), &keyboard::Key::Named(Named::Delete)).as_deref(), Some("delete"));
    }

    #[test]
    fn validate_rules() {
        assert!(validate(CommandId::NoteUndo, "mod+z").is_ok());
        assert_eq!(validate(CommandId::NoteUndo, "z"), Err("此命令需包含 Ctrl/Cmd"));
        assert!(validate(CommandId::TableClear, "delete").is_ok());
        assert!(validate(CommandId::TableClear, "shift+delete").is_ok());
        assert_eq!(validate(CommandId::TableClear, "mod+delete"), Err("此命令不得包含 Ctrl/Cmd/Alt"));
    }

    #[test]
    fn resolve_override_and_invalid_fallback() {
        let mut ov = HashMap::new();
        ov.insert("note.undo".to_string(), "mod+u".to_string());
        ov.insert("table.clear".to_string(), "mod+delete".to_string()); // 违规 → 回退默认
        let r = resolve(&ov);
        assert_eq!(r.iter().find(|(c, _)| *c == CommandId::NoteUndo).unwrap().1, "mod+u");
        assert_eq!(r.iter().find(|(c, _)| *c == CommandId::TableClear).unwrap().1, "delete");
    }

    #[test]
    fn note_bindings_apply_override_and_fallback() {
        let mut ov = HashMap::new();
        ov.insert("note.undo".to_string(), "mod+u".to_string());
        ov.insert("note.redo".to_string(), "u".to_string()); // 违规 → 回退默认
        let b = note_bindings(&ov);
        assert_eq!(b.undo, "mod+u");
        assert_eq!(b.redo, "mod+y");
        // 无覆盖 = 默认表
        let b = note_bindings(&HashMap::new());
        assert_eq!(b.undo, "mod+z");
        assert_eq!(b.redo, "mod+y");
    }

    #[test]
    fn conflicts_are_scope_local() {
        let mut ov = HashMap::new();
        ov.insert("table.undo".to_string(), "mod+c".to_string());
        let r = resolve(&ov);
        assert_eq!(conflicts(&r, CommandId::TableCopy), vec![CommandId::TableUndo]);
        // note 域同键串不与 table 冲突
        let mut ov = HashMap::new();
        ov.insert("table.undo".to_string(), "mod+z".to_string());
        let r = resolve(&ov);
        assert!(conflicts(&r, CommandId::NoteUndo).is_empty());
    }

    #[test]
    fn redo_alias_gating() {
        let mut ov = HashMap::new();
        assert!(redo_alias_active(&ov, Scope::Note));
        // redo 被覆盖 → 别名失效
        ov.insert("note.redo".to_string(), "mod+r".to_string());
        assert!(!redo_alias_active(&ov, Scope::Note));
        // undo 被改绑到 mod+shift+z → 别名失效（一键双义）
        let mut ov = HashMap::new();
        ov.insert("note.undo".to_string(), "mod+shift+z".to_string());
        assert!(!redo_alias_active(&ov, Scope::Note));
        // 作用域隔离
        assert!(redo_alias_active(&ov, Scope::Table));
    }
}
