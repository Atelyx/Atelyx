//! 笔记 frontmatter：文档开头 `---` 块的定位、宽类型解析与受限序列化。
//!
//! 解析 = yaml-rust2 事件流搭结构 + 自研 YAML 1.1 标量解析表（js-yaml v3 语义：yes/on/timestamp
//! 等形态有类型）；序列化 = 自研 emitter（js-yaml dump 形状：块列表缩进、单引号规则、字面量块），
//! 两侧语义由 `fixtures/frontmatter/parse.json` 对拍锁定。

use std::sync::OnceLock;

use regex::Regex;
use yaml_rust2::parser::{Event, Parser};
use yaml_rust2::scanner::TScalarStyle;

/// frontmatter 宽类型值（属性面板的数据真源；未知形态经解析表落到最接近的类型）。
#[derive(Clone, Debug, PartialEq)]
pub enum Yaml {
    Null,
    Bool(bool),
    Num(f64),
    /// UTC 日历日（YYYY-MM-DD）：js-yaml 把时间戳解析为 Date，此处存其 UTC 日历日。
    Date(String),
    Str(String),
    Seq(Vec<Yaml>),
    /// 键序保留（属性编辑按原序写回）。
    Map(Vec<(String, Yaml)>),
}

/// 解析结果：`data` = 键值宽类型；`fm_prefix` = 开头块原文（字节级，含闭合行与行尾换行）；
/// `body` = 块后正文；`ok` = false 表示 YAML 解析失败（正文照原文保留，不阻塞编辑）。
pub struct ParsedFrontmatter {
    pub data: Yaml,
    pub body: String,
    pub fm_prefix: String,
    pub ok: bool,
}

impl Yaml {
    /// 取映射键值对（非映射返回 None；属性面板按此遍历）。
    pub fn as_map(&self) -> Option<&[(String, Yaml)]> {
        match self {
            Yaml::Map(pairs) => Some(pairs),
            _ => None,
        }
    }

    fn empty_map() -> Yaml {
        Yaml::Map(Vec::new())
    }
}

/// 解析一篇笔记：开头 `---` 块内的 YAML → `data`，其余为 `body`。
///
/// 定界与容错对齐 gray-matter：开头行 = 文件首行以 `---` 起始（其后可有尾注）；
/// 闭合行 = 其后第一条 `---` 起始行；无闭合时 YAML 解析至文末且 body 保持全文；
/// YAML 解析失败 → `ok=false`、`data` 为空映射、body 全文原样。
pub fn parse(content: &str) -> ParsedFrontmatter {
    let Some(open_end) = content.find('\n').or_else(|| content.is_empty().then_some(0)) else {
        return ParsedFrontmatter { data: Yaml::empty_map(), body: content.into(), fm_prefix: String::new(), ok: true };
    };
    let open_line = &content[..open_end];
    if !open_line.starts_with("---") {
        return ParsedFrontmatter { data: Yaml::empty_map(), body: content.into(), fm_prefix: String::new(), ok: true };
    }
    let (close_start, close_end, closed) = find_close(content, open_end);
    let region = &content[open_end..close_start];
    match load_region(region) {
        Ok(data) => {
            if closed {
                ParsedFrontmatter { data, body: content[close_end..].into(), fm_prefix: content[..close_end].into(), ok: true }
            } else {
                ParsedFrontmatter { data, body: content.into(), fm_prefix: String::new(), ok: true }
            }
        }
        Err(_) => ParsedFrontmatter { data: Yaml::empty_map(), body: content.into(), fm_prefix: String::new(), ok: false },
    }
}

/// 把属性数据序列化为 frontmatter 块 + 正文；空映射（含非映射）不输出块，正文原样返回。
///
/// 块形状 = `---\n` + YAML + `---\n` + body；body 含 CRLF 时块内换行跟随正文风格；
/// 只编辑正文时块字节不重排（属性编辑须自行改 data 后走本函数）。
pub fn stringify(data: &Yaml, body: &str) -> String {
    let Yaml::Map(pairs) = data else { return body.to_string() };
    if pairs.is_empty() {
        return body.to_string();
    }
    let mut out = String::new();
    out.push_str("---\n");
    emit_map(&mut out, pairs, 0);
    out.push_str("---\n");
    out.push_str(body);
    if body.contains("\r\n") {
        let fm_len = out.len() - body.len();
        let fm = lf_to_crlf(&out[..fm_len]);
        out.replace_range(..fm_len, &fm);
    }
    out
}

// ===== 定位 =====

/// 文档开头 frontmatter 块的结束字节位置（闭合行行尾，含换行）；无块返回 None。
pub fn block_end(content: &str) -> Option<usize> {
    let open_end = content.find('\n')?;
    if !content[..open_end].starts_with("---") {
        return None;
    }
    let (_, close_end, closed) = find_close(content, open_end);
    closed.then_some(close_end)
}

/// 闭合行位置：`from` 起第一条 `---` 起始行；返回（闭合行起点、终点（含行尾换行）、是否闭合）。
/// 无闭合时返回文末两点与 false（YAML 解析至文末）。
fn find_close(content: &str, from: usize) -> (usize, usize, bool) {
    let mut pos = from;
    for line in content[from..].split_inclusive('\n') {
        let trimmed = line.strip_suffix('\n').unwrap_or(line);
        let trimmed = trimmed.strip_suffix('\r').unwrap_or(trimmed);
        if trimmed.starts_with("---") {
            return (pos, pos + line.len(), true);
        }
        pos += line.len();
    }
    (pos, pos, false)
}

// ===== YAML 解析 =====

/// 区域文本 → 宽类型。空区域 = 空映射（空 frontmatter 块语义）。
fn load_region(region: &str) -> Result<Yaml, String> {
    if region.trim().is_empty() {
        return Ok(Yaml::empty_map());
    }
    let mut events = Vec::new();
    let mut parser = Parser::new_from_str(region);
    loop {
        let (ev, _) = parser.next_token().map_err(|e| e.to_string())?;
        events.push(ev);
        if matches!(events.last(), Some(Event::StreamEnd)) {
            break;
        }
    }
    let mut idx = 0;
    while matches!(events.get(idx), Some(Event::StreamStart | Event::DocumentStart)) {
        idx += 1;
    }
    let value = load_node(&events, &mut idx)?;
    while matches!(events.get(idx), Some(Event::DocumentEnd | Event::StreamEnd)) {
        idx += 1;
    }
    if idx != events.len() {
        return Err("只接受单个 YAML 文档".into());
    }
    Ok(value)
}

fn load_node(events: &[Event], idx: &mut usize) -> Result<Yaml, String> {
    let Some(ev) = events.get(*idx) else {
        return Err("YAML 事件缺失".into());
    };
    *idx += 1;
    match ev {
        Event::Scalar(text, style, _, tag) => {
            if tag.is_some() {
                return Err("不支持的 YAML 标签".into());
            }
            Ok(match style {
                TScalarStyle::Plain => resolve_scalar(text),
                _ => Yaml::Str(text.clone()),
            })
        }
        Event::SequenceStart(_, tag) => {
            if tag.is_some() {
                return Err("不支持的 YAML 标签".into());
            }
            let mut items = Vec::new();
            while !matches!(events.get(*idx), Some(Event::SequenceEnd)) {
                items.push(load_node(events, idx)?);
            }
            *idx += 1;
            Ok(Yaml::Seq(items))
        }
        Event::MappingStart(_, tag) => {
            if tag.is_some() {
                return Err("不支持的 YAML 标签".into());
            }
            let mut pairs: Vec<(String, Yaml)> = Vec::new();
            while !matches!(events.get(*idx), Some(Event::MappingEnd)) {
                let key = load_key(events, idx)?;
                let value = load_node(events, idx)?;
                pairs.retain(|(k, _)| k != &key);
                pairs.push((key, value));
            }
            *idx += 1;
            Ok(Yaml::Map(pairs))
        }
        Event::Alias(_) => Err("不支持的 YAML 锚点引用".into()),
        _ => Err("意外的 YAML 事件".into()),
    }
}

/// 映射键：取标量原文（键不做类型解析，一律按文本）。
fn load_key(events: &[Event], idx: &mut usize) -> Result<String, String> {
    let Some(ev) = events.get(*idx) else {
        return Err("YAML 事件缺失".into());
    };
    *idx += 1;
    match ev {
        Event::Scalar(text, _, _, _) => Ok(text.clone()),
        _ => Err("映射键必须是标量".into()),
    }
}

/// YAML 1.1 标量解析表（js-yaml safe schema 语义）：null → bool → int → float → timestamp → 字符串。
/// 仅用于无引号标量；带引号标量恒为字符串。
fn resolve_scalar(s: &str) -> Yaml {
    if matches!(s, "" | "~" | "null" | "Null" | "NULL") {
        return Yaml::Null;
    }
    if matches!(
        s,
        "y" | "Y" | "yes" | "Yes" | "YES" | "true" | "True" | "TRUE" | "on" | "On" | "ON"
    ) {
        return Yaml::Bool(true);
    }
    if matches!(
        s,
        "n" | "N" | "no" | "No" | "NO" | "false" | "False" | "FALSE" | "off" | "Off" | "OFF"
    ) {
        return Yaml::Bool(false);
    }
    if let Some(v) = parse_int(s) {
        return Yaml::Num(v as f64);
    }
    if let Some(v) = parse_float(s) {
        return Yaml::Num(v);
    }
    if let Some(d) = timestamp_to_date(s) {
        return Yaml::Date(d);
    }
    Yaml::Str(s.into())
}

/// YAML 1.1 整数：十进制（含下划线）/ 0x 十六进制 / 0b 二进制 / 0 前缀八进制。
fn parse_int(s: &str) -> Option<i64> {
    let (neg, rest) = match s.strip_prefix(|c| c == '-' || c == '+') {
        Some(r) => (s.starts_with('-'), r),
        None => (false, s),
    };
    if rest.is_empty() {
        return None;
    }
    let (digits, radix) = if let Some(r) = rest.strip_prefix("0b") {
        (r, 2u32)
    } else if let Some(r) = rest.strip_prefix("0x") {
        (r, 16)
    } else if rest.len() > 1 && rest.starts_with('0') {
        let r = &rest[1..];
        if !r.chars().all(|c| matches!(c, '0'..='7' | '_')) {
            return None;
        }
        (r, 8)
    } else if rest == "0"
        || (rest.starts_with(|c: char| ('1'..='9').contains(&c)) && rest.chars().all(|c| c.is_ascii_digit() || c == '_'))
    {
        (rest, 10)
    } else {
        return None;
    };
    let cleaned: String = digits.chars().filter(|c| *c != '_').collect();
    if cleaned.is_empty() {
        return None;
    }
    i64::from_str_radix(&cleaned, radix).ok().map(|v| if neg { -v } else { v })
}

/// YAML 1.1 浮点：带小数点或指数的数字形态与 .inf / .nan（纯整数由 parse_int 接管）。
fn parse_float(s: &str) -> Option<f64> {
    let (neg, rest) = match s.strip_prefix(|c| c == '-' || c == '+') {
        Some(r) => (s.starts_with('-'), r),
        None => (false, s),
    };
    match rest {
        ".inf" | ".Inf" | ".INF" => return Some(if neg { f64::NEG_INFINITY } else { f64::INFINITY }),
        ".nan" | ".NaN" | ".NAN" => return Some(f64::NAN),
        _ => {}
    }
    let (mantissa, exponent) = match rest.split_once(['e', 'E']) {
        Some((m, e)) => (m, Some(e)),
        None => (rest, None),
    };
    let mantissa = mantissa.replace('_', "");
    let (int_part, frac_part) = match mantissa.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (mantissa.as_str(), None),
    };
    let int_ok = int_part.chars().all(|c| c.is_ascii_digit()) && !int_part.is_empty();
    let frac_ok = frac_part.is_none_or(|f| f.chars().all(|c| c.is_ascii_digit()));
    let exp_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        !e.is_empty() && e.chars().all(|c| c.is_ascii_digit())
    });
    let has_dot = frac_part.is_some();
    if !int_ok || !frac_ok || !exp_ok || (!has_dot && exponent.is_none()) {
        return None;
    }
    let v: f64 = s.replace('_', "").parse().ok()?;
    Some(if neg { -v } else { v })
}

/// 时间戳 → UTC 日历日（YYYY-MM-DD）。日期部分原样进位归一（13 月 / 超日进位同 JS Date 语义）；
/// 带时间部分按 Z 或 ±时区换算到 UTC，无时区视同 UTC（js-yaml v3 行为）。
fn timestamp_to_date(s: &str) -> Option<String> {
    fn re() -> &'static Regex {
        static TS: OnceLock<Regex> = OnceLock::new();
        TS.get_or_init(|| Regex::new(
            r"^(\d{4})-(\d{1,2})-(\d{1,2})(?:(?:[Tt]|[ \t]+)(\d{1,2}):(\d{2}):(\d{2})(?:\.(\d*))?(?:[ \t]*(Z|[-+])(\d{1,2})(?::(\d{2}))?)?)?$",
        ).unwrap())
    }
    let caps = re().captures(s)?;
    let year: i64 = caps[1].parse().ok()?;
    let mut month: i64 = caps[2].parse().ok()?;
    let day: i64 = caps[3].parse().ok()?;
    let mut y = year;
    if !(1..=12).contains(&month) {
        y += (month - 1).div_euclid(12);
        month = (month - 1).rem_euclid(12) + 1;
    }
    let mut total = days_from_civil(y, month, 1) + (day - 1);
    if let (Some(h), Some(mi), Some(sec)) = (caps.get(4), caps.get(5), caps.get(6)) {
        let secs = h.as_str().parse::<i64>().ok()? * 3600
            + mi.as_str().parse::<i64>().ok()? * 60
            + sec.as_str().parse::<i64>().ok()?;
        total = total * 86400 + secs;
        if let Some(z) = caps.get(8) {
            let offset: i64 = if z.as_str() == "Z" {
                0
            } else {
                let sign: i64 = if z.as_str() == "-" { -1 } else { 1 };
                let tzh: i64 = caps.get(9)?.as_str().parse().ok()?;
                let tzm: i64 = caps.get(10).and_then(|m| m.as_str().parse().ok()).unwrap_or(0);
                sign * (tzh * 3600 + tzm * 60)
            };
            total -= offset;
        }
        total = total.div_euclid(86400);
    }
    let (ry, rm, rd) = civil_from_days(total);
    Some(format!("{ry:04}-{rm:02}-{rd:02}"))
}

/// 格里历日数 ↔ 日期（Howard Hinnant 算法；day 0 = 1970-01-01）。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (y + i64::from(m <= 2), m, d)
}

// ===== 序列化（js-yaml dump 形状）=====

fn emit_map(out: &mut String, pairs: &[(String, Yaml)], indent: usize) {
    for (key, value) in pairs {
        push_indent(out, indent);
        out.push_str(&string_token(key));
        match value {
            Yaml::Seq(items) if items.is_empty() => out.push_str(": []\n"),
            Yaml::Map(p) if p.is_empty() => out.push_str(": {}\n"),
            Yaml::Str(s) if s.contains('\n') => {
                out.push_str(": ");
                emit_literal_safe(out, s, indent + 2);
            }
            Yaml::Seq(_) | Yaml::Map(_) => {
                out.push_str(":\n");
                let mut child = String::new();
                emit_block(&mut child, value, indent + 2);
                out.push_str(&child);
            }
            other => {
                out.push_str(": ");
                out.push_str(&scalar_token(other));
                out.push('\n');
            }
        }
    }
}

fn emit_seq_item(out: &mut String, item: &Yaml, indent: usize) {
    push_indent(out, indent);
    out.push_str("- ");
    match item {
        Yaml::Seq(items) if items.is_empty() => out.push_str("[]\n"),
        Yaml::Map(p) if p.is_empty() => out.push_str("{}\n"),
        Yaml::Str(s) if s.contains('\n') => {
            emit_literal_safe(out, s, indent + 2);
        }
        Yaml::Seq(_) | Yaml::Map(_) => {
            // 集合子项首行骑在 `- ` 上，后继行对齐到内容列（dash + 2）
            let mut child = String::new();
            emit_block(&mut child, item, indent + 2);
            out.push_str(&child[indent + 2..]);
        }
        other => {
            out.push_str(&scalar_token(other));
            out.push('\n');
        }
    }
}

fn emit_block(out: &mut String, value: &Yaml, indent: usize) {
    match value {
        Yaml::Map(pairs) => emit_map(out, pairs, indent),
        Yaml::Seq(items) => {
            for item in items {
                emit_seq_item(out, item, indent);
            }
        }
        _ => {}
    }
}

/// 多行字符串的字面量块（js-yaml v3 缺省形态）：|-(无尾换行) / |(恰一个) / |+(多个)。
/// 内容存在行首空白（空格/制表符）时不可用——块缩进按首行取，后继行缩进不足会破坏
/// YAML 结构，调用方须回落双引号标量（见 emit_literal_safe）。
fn emit_literal(out: &mut String, s: &str, indent: usize) {
    let (header, content) = if !s.ends_with('\n') {
        ("|-", s)
    } else if s.ends_with("\n\n") {
        ("|+", &s[..s.len() - 1])
    } else {
        ("|", &s[..s.len() - 1])
    };
    out.push_str(header);
    out.push('\n');
    for line in content.split('\n') {
        push_indent(out, indent);
        out.push_str(line);
        out.push('\n');
    }
}

/// 多行字符串标量：无行首空白走字面量块（js-yaml 缺省），否则双引号 + 转义（字面量块
/// 对行首空白不安全，单引号/plain 无法承载换行）。
fn emit_literal_safe(out: &mut String, s: &str, indent: usize) {
    let literal_ok = s.split('\n').all(|line| !line.starts_with([' ', '\t']));
    if literal_ok {
        emit_literal(out, s, indent);
    } else {
        let mut escaped = String::with_capacity(s.len() + 8);
        for c in s.chars() {
            match c {
                '"' => escaped.push_str("\\\""),
                '\\' => escaped.push_str("\\\\"),
                '\n' => escaped.push_str("\\n"),
                '\t' => escaped.push_str("\\t"),
                '\r' => escaped.push_str("\\r"),
                other => escaped.push(other),
            }
        }
        out.push_str(&format!("\"{escaped}\"\n"));
    }
}

fn push_indent(out: &mut String, indent: usize) {
    for _ in 0..indent {
        out.push(' ');
    }
}

/// 标量 token：非字符串类型一律 plain（js-yaml 按类型转义），字符串走 plain/引号判定。
fn scalar_token(v: &Yaml) -> String {
    match v {
        Yaml::Null => "null".into(),
        Yaml::Bool(b) => if *b { "true" } else { "false" }.into(),
        Yaml::Num(f) => js_num(*f),
        Yaml::Date(d) => format!("{d}T00:00:00.000Z"),
        Yaml::Str(s) => string_token(s),
        Yaml::Seq(_) | Yaml::Map(_) => String::new(),
    }
}

fn string_token(s: &str) -> String {
    if plain_ok(s) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "''"))
    }
}

/// plain 形态判定（js-yaml dumper 规则）：空、首尾空白、指示符开头、含冒号/制表符/" #"、
/// 或命中 1.1 解析表（会被读侧转成非字符串）→ 必须加引号。
fn plain_ok(s: &str) -> bool {
    if s.is_empty() || s.trim() != s || s.contains('\t') {
        return false;
    }
    let first = s.chars().next().unwrap();
    if matches!(first, '#' | ',' | '[' | ']' | '{' | '}' | '&' | '*' | '!' | '|' | '>' | '\'' | '"' | '%' | '@' | '`') {
        return false;
    }
    if matches!(first, '-' | '?' | ':') {
        let rest = &s[first.len_utf8()..];
        if rest.is_empty() || rest.starts_with([' ', '\t']) {
            return false;
        }
    }
    if s.contains(':') || s.contains(" #") {
        return false;
    }
    if !matches!(resolve_scalar(s), Yaml::Str(_)) {
        return false;
    }
    true
}

/// 数字文本（JS Number→string 语义）：整数值不带小数点；绝对值 ≥1e21 或 <1e-6 用指数记法。
fn js_num(v: f64) -> String {
    if v.is_nan() {
        return ".nan".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { ".inf".into() } else { "-.inf".into() };
    }
    if v.abs() >= 1e21 || (v != 0.0 && v.abs() < 1e-6) {
        let s = format!("{v:e}");
        let (mantissa, exp) = s.split_once('e').unwrap();
        let exp: i64 = exp.parse().unwrap();
        return if exp >= 0 {
            format!("{mantissa}e+{exp:02}")
        } else {
            format!("{mantissa}e-{}", -exp)
        };
    }
    format!("{v}")
}

/// 块内换行跟随正文 CRLF 风格（逐个 `\n` 转 `\r\n`，`\r\n` 保持不变）。
fn lf_to_crlf(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 8);
    let chars = s.char_indices().peekable();
    for (i, c) in chars {
        if c == '\n' && (i == 0 || !s[..i].ends_with('\r')) {
            out.push('\r');
        }
        out.push(c);
    }
    out
}

// ===== 属性类型推断与转换（面板类型切换语义；转换失败返回 None = 保留原值）=====

/// 属性值类型（YAML 值即类型，无独立 schema）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PropType {
    Text,
    Tag,
    Date,
    Number,
    Boolean,
    List,
}

/// 按值推断当前类型：数组按徽章键区分标签/列表，其余按值形态。
pub fn infer_type(badge_key: bool, value: &Yaml) -> PropType {
    match value {
        Yaml::Seq(_) if badge_key => PropType::Tag,
        Yaml::Seq(_) => PropType::List,
        Yaml::Date(_) => PropType::Date,
        Yaml::Num(_) => PropType::Number,
        Yaml::Bool(_) => PropType::Boolean,
        _ => PropType::Text,
    }
}

/// 把值转换为目标类型；不可转换返回 None（调用方保留原值，防破坏 YAML 数据）。
pub fn convert_value(value: &Yaml, target: PropType) -> Option<Yaml> {
    match target {
        PropType::Text => Some(Yaml::Str(match value {
            Yaml::Null => String::new(),
            Yaml::Date(d) => d.clone(),
            Yaml::Num(f) => js_num(*f),
            Yaml::Bool(b) => if *b { "true" } else { "false" }.into(),
            Yaml::Str(s) => s.clone(),
            Yaml::Seq(items) => items
                .iter()
                .map(item_text)
                .collect::<Vec<_>>()
                .join("，"),
            Yaml::Map(_) => "[object Object]".into(),
        })),
        PropType::Tag | PropType::List => {
            let items: Vec<String> = match value {
                Yaml::Null => Vec::new(),
                Yaml::Seq(items) => items.iter().map(item_text).collect(),
                other => vec![item_text(other)],
            };
            Some(Yaml::Seq(
                items.into_iter().filter(|s| !s.is_empty()).map(Yaml::Str).collect(),
            ))
        }
        PropType::Date => match value {
            Yaml::Date(d) => Some(Yaml::Date(d.clone())),
            Yaml::Num(f) if f.fract() == 0.0 => {
                let days = (*f as i64).div_euclid(86400);
                let (y, m, d) = civil_from_days(days);
                Some(Yaml::Date(format!("{y:04}-{m:02}-{d:02}")))
            }
            Yaml::Str(s) if !s.trim().is_empty() => timestamp_to_date(s.trim()).map(Yaml::Date),
            _ => None,
        },
        PropType::Number => match value {
            Yaml::Num(f) => Some(Yaml::Num(*f)),
            Yaml::Bool(b) => Some(Yaml::Num(if *b { 1.0 } else { 0.0 })),
            Yaml::Str(s) => s.trim().parse::<f64>().ok().map(Yaml::Num),
            _ => None,
        },
        PropType::Boolean => match value {
            Yaml::Bool(b) => Some(Yaml::Bool(*b)),
            Yaml::Num(f) => Some(Yaml::Bool(*f != 0.0)),
            Yaml::Str(s) => match s.trim().to_ascii_lowercase().as_str() {
                "true" | "yes" | "1" => Some(Yaml::Bool(true)),
                "false" | "no" | "0" => Some(Yaml::Bool(false)),
                _ => None,
            },
            _ => None,
        },
    }
}

/// 数组项文本化（Date → 日历日，其余按显示形态字符串化）。
fn item_text(v: &Yaml) -> String {
    match v {
        Yaml::Null => String::new(),
        Yaml::Date(d) => d.clone(),
        Yaml::Num(f) => js_num(*f),
        Yaml::Bool(b) => if *b { "true" } else { "false" }.into(),
        Yaml::Str(s) => s.clone(),
        Yaml::Seq(_) | Yaml::Map(_) => "[object Object]".into(),
    }
}
