//! 表格 AI 字段：模板字段引用解析、提示词渲染、输入指纹与单元格状态判定。
//! 纯逻辑无 I/O；系统提示词笔记正文由调用方在生成前解析。

use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::vault::{AiFieldConfig, AiSystem, TableField, TableRow};

/// AI 字段类型值。
pub const FIELD_TYPE: &str = "ai";
/// 单个引用值代入上限（字符；超出截断并带省略号）。
pub const VALUE_CHAR_CAP: usize = 2000;
/// 空值代入占位。
pub const EMPTY_VALUE: &str = "（空）";
/// 图片列占位（生成请求不发送图片）。
pub const IMAGE_VALUE: &str = "（图片）";

/// 模板里的一次字段引用（字节区间，含起始 `#`）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AiRef {
    pub field_id: String,
    pub start: usize,
    pub end: usize,
}

/// 字段是否为 AI 类型。
pub fn is_ai(field: &TableField) -> bool {
    field.field_type == FIELD_TYPE
}

/// 解析模板引用：`#字段名` 按「`#` 之后最长字段名前缀」匹配，因此 `#画面描述和#镜头` 这类
/// 无分隔写法也能正确断词；同名字段同时命中视为歧义（返回可读错误，不猜）。
pub fn parse_refs(prompt: &str, fields: &[TableField]) -> Result<Vec<AiRef>, String> {
    let mut refs = Vec::new();
    let mut i = 0;
    while i < prompt.len() {
        let rest = &prompt[i..];
        let Some(ch) = rest.chars().next() else { break };
        if ch != '#' {
            i += ch.len_utf8();
            continue;
        }
        let tail = &prompt[i + 1..];
        let mut best: Option<(&TableField, usize)> = None;
        let mut duplicated = false;
        for field in fields {
            if is_ai(field) || field.name.is_empty() || !tail.starts_with(&field.name) {
                continue;
            }
            let len = field.name.len();
            match best {
                None => best = Some((field, len)),
                Some((_, best_len)) if len > best_len => {
                    best = Some((field, len));
                    duplicated = false;
                }
                Some((current, best_len)) if len == best_len && current.id != field.id => {
                    duplicated = true;
                }
                _ => {}
            }
        }
        let Some((field, len)) = best else {
            i += 1;
            continue;
        };
        if duplicated {
            return Err(format!(
                "模板里的 #{} 对应多个同名字段，请先改掉重复的字段名",
                field.name
            ));
        }
        refs.push(AiRef { field_id: field.id.clone(), start: i, end: i + 1 + len });
        i += 1 + len;
    }
    Ok(refs)
}

/// 字段配置（非 AI 字段 / 未配置返回可读错误）。
pub fn config_of(field: &TableField) -> Result<&AiFieldConfig, String> {
    if !is_ai(field) {
        return Err("字段不是 AI 字段".to_string());
    }
    field.ai.as_ref().ok_or_else(|| "AI 字段缺少配置，请先在字段设置里填写".to_string())
}

/// 校验字段配置，返回模板引用。
pub fn validate(field: &TableField, fields: &[TableField]) -> Result<Vec<AiRef>, String> {
    let config = config_of(field)?;
    if config.prompt.trim().is_empty() {
        return Err("指令模板不能为空".to_string());
    }
    if config.prompt.chars().count() > 4000 {
        return Err("指令模板过长（上限 4000 字）".to_string());
    }
    if let AiSystem::Note { path } = &config.system {
        if path.trim().is_empty() {
            return Err("系统提示词笔记未选择".to_string());
        }
    }
    let refs = parse_refs(&config.prompt, fields)?;
    if refs.is_empty() {
        return Err(if config.prompt.contains('#') {
            "模板引用的字段已不存在（可能已删除或改名）".to_string()
        } else {
            "指令模板至少引用一列（输入 # 选择字段）".to_string()
        });
    }
    Ok(refs)
}

/// 单元格值 → 代入文本（按显示口径；空值与图片给占位，超长截断）。
pub fn cell_text(field: &TableField, row: &TableRow) -> String {
    match row.values.get(&field.id) {
        None | Some(Value::Null) => EMPTY_VALUE.to_string(),
        Some(Value::String(text)) if text.trim().is_empty() => EMPTY_VALUE.to_string(),
        Some(Value::String(text)) => truncate(text),
        Some(Value::Number(number)) => {
            let text = number
                .as_f64()
                .map(format_number)
                .unwrap_or_else(|| number.to_string());
            if field.field_type == "duration" {
                format!("{text} 秒")
            } else {
                text
            }
        }
        Some(value) if field.field_type == "image" => {
            let _ = value;
            IMAGE_VALUE.to_string()
        }
        Some(value) => truncate(&value.to_string()),
    }
}

fn format_number(number: f64) -> String {
    if number.fract() == 0.0 && number.abs() < 9.0e15 {
        format!("{}", number as i64)
    } else {
        format!("{number}")
    }
}

fn truncate(text: &str) -> String {
    if text.chars().count() <= VALUE_CHAR_CAP {
        return text.to_string();
    }
    let mut out: String = text.chars().take(VALUE_CHAR_CAP).collect();
    out.push('…');
    out
}

/// 模板渲染：引用替换为「字段值」，其余原文保留。解析失败时原样返回（调用方先校验）。
pub fn render(prompt: &str, fields: &[TableField], row: &TableRow) -> String {
    let Ok(refs) = parse_refs(prompt, fields) else {
        return prompt.to_string();
    };
    let mut out = String::with_capacity(prompt.len() + 64);
    let mut cursor = 0;
    for reference in refs {
        if reference.start < cursor {
            continue;
        }
        out.push_str(&prompt[cursor..reference.start]);
        let Some(field) = fields.iter().find(|f| f.id == reference.field_id) else {
            cursor = reference.end;
            continue;
        };
        out.push('「');
        out.push_str(&cell_text(field, row));
        out.push('」');
        cursor = reference.end;
    }
    out.push_str(&prompt[cursor..]);
    out
}

/// 引用列在本行是否全为空（全空 = 无输入，不发起请求）。
pub fn no_input(fields: &[TableField], refs: &[AiRef], row: &TableRow) -> bool {
    refs.iter().all(|reference| {
        fields
            .iter()
            .find(|f| f.id == reference.field_id)
            .is_none_or(|field| cell_text(field, row) == EMPTY_VALUE)
    })
}


/// 模板引用归一化：把 #字段名 替换成 #{字段 id}，其余文本原样保留。
fn normalize_refs(prompt: &str, fields: &[TableField]) -> String {
    let Ok(refs) = parse_refs(prompt, fields) else {
        return prompt.to_string();
    };
    let mut out = String::with_capacity(prompt.len());
    let mut cursor = 0;
    for reference in refs {
        if reference.start < cursor {
            continue;
        }
        out.push_str(&prompt[cursor..reference.start]);
        out.push('#');
        out.push('{');
        out.push_str(&reference.field_id);
        out.push('}');
        cursor = reference.end;
    }
    out.push_str(&prompt[cursor..]);
    out
}

/// 输入指纹：系统提示词描述 + 模板 + 输出规格 + 各引用值。
/// 只依赖字段配置与行内容（系统提示词笔记用路径占位），随时可重算，用于过期判定与陈旧结果丢弃。
pub fn fingerprint(field: &TableField, fields: &[TableField], row: &TableRow) -> Option<String> {
    fingerprint_with_config(config_of(field).ok()?, fields, row)
}


/// 影响模型输出的配置是否一致（自动开关不改变输出，不应作废在途结果）。
pub fn same_output_config(a: &AiFieldConfig, b: &AiFieldConfig) -> bool {
    a.system == b.system
        && a.prompt == b.prompt
        && model_descriptor(a) == model_descriptor(b)
}

/// 指定模型的指纹描述；跟随默认返回 None（默认模型在设置里切换不追溯既有结果）。
fn model_descriptor(config: &AiFieldConfig) -> Option<String> {
    match (config.provider_id.as_deref(), config.model.as_deref()) {
        (Some(provider), Some(model)) if !provider.is_empty() && !model.is_empty() => {
            Some(format!("{provider}/{model}"))
        }
        _ => None,
    }
}

/// 以冻结的配置计算指纹（批次在途时用发起时的配置，避免配置中途变更导致口径不一致）。
pub fn fingerprint_with_config(
    config: &AiFieldConfig,
    fields: &[TableField],
    row: &TableRow,
) -> Option<String> {
    let refs = parse_refs(&config.prompt, fields).ok()?;
    if refs.is_empty() {
        return None;
    }
    let mut hasher = Sha256::new();
    hasher.update(system_descriptor(&config.system).as_bytes());
    hasher.update([0]);
    // 引用按字段 id 归一：改字段名（模板同步改写）不改变输入语义，不判过期
    hasher.update(normalize_refs(&config.prompt, fields).as_bytes());
    hasher.update([0]);
    hasher.update(crate::vault::AI_OUTPUT_LONG.as_bytes());
    if let Some(model) = model_descriptor(config) {
        hasher.update([0]);
        hasher.update(model.as_bytes());
    }
    for reference in &refs {
        let field = fields.iter().find(|f| f.id == reference.field_id)?;
        hasher.update([0]);
        hasher.update(field.id.as_bytes());
        hasher.update([b'=']);
        hasher.update(cell_text(field, row).as_bytes());
    }
    Some(hex::encode(hasher.finalize())[..16].to_string())
}

/// 系统提示词的可重算描述（指纹与预览共用；笔记只记路径，正文改动不自动追溯过期）。
pub fn system_descriptor(system: &AiSystem) -> String {
    match system {
        AiSystem::None => "none".to_string(),
        AiSystem::Inline { text } => format!("inline:{text}"),
        AiSystem::Note { path } => format!("note:{path}"),
    }
}

/// 单元格状态（绘制与批量筛选共用）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AiCellState {
    /// 字段配置缺失 / 无效。
    Unconfigured,
    /// 本端有在途请求。
    Running,
    /// 上次生成失败（保留旧值）。
    Failed(String),
    /// 引用列全空。
    NoInput,
    /// 未生成。
    Empty,
    /// 已生成且与当前输入一致。
    Ready,
    /// 已生成但输入已变。
    Stale,
}

/// 单元格状态：运行态优先，其次失败态，最后按值与指纹判定。
pub fn cell_state(field: &TableField, fields: &[TableField], row: &TableRow, running: bool) -> AiCellState {
    if running {
        return AiCellState::Running;
    }
    let Ok(config) = config_of(field) else {
        return AiCellState::Unconfigured;
    };
    if let Some(meta) = row.ai.as_ref().and_then(|m| m.get(&field.id)) {
        if meta.status == "failed" {
            return AiCellState::Failed(meta.error.clone().unwrap_or_default());
        }
    }
    let refs = parse_refs(&config.prompt, fields).unwrap_or_default();
    if refs.is_empty() {
        return AiCellState::Unconfigured;
    }
    let value = row.values.get(&field.id).and_then(Value::as_str).unwrap_or("");
    if value.is_empty() {
        return if no_input(fields, &refs, row) { AiCellState::NoInput } else { AiCellState::Empty };
    }
    match row.ai.as_ref().and_then(|m| m.get(&field.id)) {
        Some(meta) if meta.status == "ok" && Some(&meta.input) == fingerprint(field, fields, row).as_ref() => {
            AiCellState::Ready
        }
        _ => AiCellState::Stale,
    }
}

/// 过期判定：失败格不算过期（按重试处理）；有值但无生成记录（如文本列转 AI）视为过期。
pub fn stale(field: &TableField, fields: &[TableField], row: &TableRow) -> bool {
    let has_value = row
        .values
        .get(&field.id)
        .and_then(Value::as_str)
        .is_some_and(|text| !text.is_empty());
    let Some(meta) = row.ai.as_ref().and_then(|m| m.get(&field.id)) else {
        return has_value;
    };
    if meta.status != "ok" {
        return false;
    }
    match fingerprint(field, fields, row) {
        Some(current) => current != meta.input,
        None => true,
    }
}

/// 把模板里指向 `field_id` 的引用改写成新字段名（字段改名时同步，按旧字段表断词）。
pub fn retarget_refs(
    prompt: &str,
    old_fields: &[TableField],
    field_id: &str,
    new_name: &str,
) -> String {
    let Ok(refs) = parse_refs(prompt, old_fields) else {
        return prompt.to_string();
    };
    let mut out = String::with_capacity(prompt.len());
    let mut cursor = 0;
    for reference in refs {
        if reference.field_id != field_id || reference.start < cursor {
            continue;
        }
        out.push_str(&prompt[cursor..reference.start]);
        out.push('#');
        out.push_str(new_name);
        cursor = reference.end;
    }
    out.push_str(&prompt[cursor..]);
    out
}

/// 批量生成筛选口径。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AiRowFilter {
    All,
    Empty,
    Stale,
    Failed,
}

/// 按筛选口径取行 id（配置无效返回空）。
pub fn filter_rows(field: &TableField, fields: &[TableField], rows: &[TableRow], filter: AiRowFilter) -> Vec<String> {
    let Ok(refs) = validate(field, fields) else {
        return Vec::new();
    };
    rows.iter()
        .filter(|row| {
            let has_value = row
                .values
                .get(&field.id)
                .and_then(Value::as_str)
                .is_some_and(|text| !text.is_empty());
            match filter {
                AiRowFilter::All => true,
                AiRowFilter::Empty => !has_value,
                AiRowFilter::Stale => stale(field, fields, row),
                AiRowFilter::Failed => {
                    row.ai
                        .as_ref()
                        .and_then(|m| m.get(&field.id))
                        .is_some_and(|meta| meta.status == "failed")
                }
            }
        })
        .filter(|row| !no_input(fields, &refs, row))
        .map(|row| row.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::AiCellMeta;
    use serde_json::json;

    fn field(id: &str, name: &str, field_type: &str) -> TableField {
        TableField {
            id: id.into(),
            name: name.into(),
            field_type: field_type.into(),
            options: None,
            width: None,
            calc_type: None,
            ai: None,
        }
    }

    fn ai_field(prompt: &str) -> TableField {
        let mut f = field("f5", "提示词", "ai");
        f.ai = Some(AiFieldConfig { prompt: prompt.into(), ..Default::default() });
        f
    }

    fn row(values: &[(&str, serde_json::Value)]) -> TableRow {
        TableRow {
            id: "r1".into(),
            values: values.iter().map(|(k, v)| ((*k).to_string(), v.clone())).collect(),
            height: None,
            styles: None,
            ai: None,
        }
    }

    #[test]
    fn refs_break_on_next_hash_without_spaces() {
        let fields = vec![field("f1", "画面描述", "text"), field("f2", "镜头", "text")];
        let prompt = "请根据#画面描述和#镜头写提示词";
        let refs = parse_refs(prompt, &fields).expect("解析");
        assert_eq!(refs.len(), 2);
        assert_eq!(&prompt[refs[0].start..refs[0].end], "#画面描述");
        assert_eq!(&prompt[refs[1].start..refs[1].end], "#镜头");
    }

    #[test]
    fn refs_pick_longest_field_name() {
        let fields = vec![field("f1", "画面", "text"), field("f2", "画面描述", "text")];
        let prompt = "#画面描述的细节，再参考#画面";
        let refs = parse_refs(prompt, &fields).expect("解析");
        assert_eq!(refs[0].field_id, "f2");
        assert_eq!(refs[1].field_id, "f1");
    }


    #[test]
    fn longer_unique_name_wins_over_duplicate_short_names() {
        let fields = vec![
            field("f1", "画面", "text"),
            field("f2", "画面", "text"),
            field("f3", "画面描述", "text"),
        ];
        let refs = parse_refs("#画面描述", &fields).expect("更长唯一名应胜出");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].field_id, "f3");
    }

    #[test]
    fn duplicate_field_names_are_rejected() {
        let fields = vec![field("f1", "镜头", "text"), field("f2", "镜头", "text")];
        let err = parse_refs("#镜头", &fields).expect_err("重名应报错");
        assert!(err.contains("同名"));
    }

    #[test]
    fn render_quotes_values_and_marks_empty_and_image() {
        let fields = vec![
            field("f1", "画面", "text"),
            field("f2", "镜头", "text"),
            field("f3", "图", "image"),
        ];
        let row = row(&[
            ("f1", json!("黄昏的街道")),
            ("f3", json!({ "images": ["a.png"] })),
        ]);
        assert_eq!(
            render("#画面 / #镜头 / #图", &fields, &row),
            "「黄昏的街道」 / 「（空）」 / 「（图片）」"
        );
    }

    #[test]
    fn fingerprint_changes_with_source_value_and_prompt() {
        let mut target = ai_field("#画面");
        let fields = vec![field("f1", "画面", "text")];
        let first = row(&[("f1", json!("甲"))]);
        let second = row(&[("f1", json!("乙"))]);
        let a = fingerprint(&target, &fields, &first).expect("指纹");
        let b = fingerprint(&target, &fields, &second).expect("指纹");
        assert_ne!(a, b);
        target.ai.as_mut().unwrap().prompt = "#画面，英文".into();
        assert_ne!(a, fingerprint(&target, &fields, &first).expect("指纹"));
    }

    #[test]
    fn fingerprint_changes_with_explicit_model() {
        let mut target = ai_field("#画面");
        let fields = vec![field("f1", "画面", "text")];
        let row = row(&[("f1", json!("甲"))]);
        let follow_default = fingerprint(&target, &fields, &row).expect("指纹");
        let config = target.ai.as_mut().expect("配置");
        config.provider_id = Some("p1".into());
        config.model = Some("m1".into());
        assert_ne!(follow_default, fingerprint(&target, &fields, &row).expect("指纹"));
        // 同一显式模型不变；只换模型即视为输入变化
        let with_m1 = fingerprint(&target, &fields, &row).expect("指纹");
        target.ai.as_mut().unwrap().model = Some("m2".into());
        assert_ne!(with_m1, fingerprint(&target, &fields, &row).expect("指纹"));
    }

    #[test]
    fn rename_retargets_refs_without_touching_other_text() {
        let old_fields = vec![field("f1", "画面描述", "text"), field("f2", "镜头", "text")];
        let prompt = "根据#画面描述和#镜头写";
        assert_eq!(
            retarget_refs(prompt, &old_fields, "f1", "画面"),
            "根据#画面和#镜头写"
        );
    }

    #[test]
    fn ai_field_cannot_be_referenced() {
        let fields = vec![field("f1", "提示词", "ai")];
        assert!(parse_refs("#提示词", &fields).expect("解析").is_empty());
    }

    #[test]
    fn no_input_when_all_refs_empty() {
        let fields = vec![field("f1", "甲", "text"), field("f2", "乙", "text")];
        let target = ai_field("#甲 与 #乙");
        let refs = validate(&target, &fields).expect("校验");
        assert!(no_input(&fields, &refs, &row(&[])));
        assert!(!no_input(&fields, &refs, &row(&[("f2", json!("有值"))])));
    }
    #[test]
    fn config_serde_keeps_wire_shape() {
        let config = AiFieldConfig {
            system: AiSystem::Note { path: "提示词/分镜.md".into() },
            prompt: "#画面".into(),
            provider_id: Some("p1".into()),
            model: Some("m1".into()),
            auto_new_row: true,
            auto_source_change: false,
        };
        let value = serde_json::to_value(&config).expect("序列化");
        assert_eq!(value["system"]["kind"], serde_json::json!("note"));
        assert_eq!(value["system"]["path"], serde_json::json!("提示词/分镜.md"));
        assert_eq!(value["providerId"], serde_json::json!("p1"));
        assert_eq!(value["model"], serde_json::json!("m1"));
        assert_eq!(value["autoNewRow"], serde_json::json!(true));
        assert_eq!(value["autoSourceChange"], serde_json::json!(false));
        assert!(value.get("output").is_none(), "输出规格不再落盘");
        let back: AiFieldConfig = serde_json::from_value(value).expect("反序列化");
        assert_eq!(back, config);
    }

    /// 存量文件里的输出规格键按未知字段忽略：配置照常读出，不因该键失败。
    #[test]
    fn legacy_output_key_is_ignored() {
        let legacy = serde_json::json!({ "prompt": "#画面", "output": "short" });
        let back: AiFieldConfig = serde_json::from_value(legacy).expect("存量配置可读");
        assert_eq!(back.prompt, "#画面");
    }

    #[test]
    fn cell_meta_omits_ok_status_and_keeps_failure() {
        let ok = AiCellMeta::ok("abc".into(), 1, Some("m".into()));
        let value = serde_json::to_value(&ok).expect("序列化");
        assert!(value.get("status").is_none());
        let failed = AiCellMeta::failed("abc".into(), 2, None, "超时".into());
        let value = serde_json::to_value(&failed).expect("序列化");
        assert_eq!(value["status"], serde_json::json!("failed"));
        assert_eq!(value["error"], serde_json::json!("超时"));
    }
    #[test]
    fn cell_state_covers_empty_stale_and_failed() {
        let fields = vec![field("f1", "画面", "text")];
        let target = ai_field("#画面");
        let mut row = row(&[("f1", json!("甲"))]);
        assert_eq!(cell_state(&target, &fields, &row, false), AiCellState::Empty);
        let input = fingerprint(&target, &fields, &row).expect("指纹");
        row.values.insert("f5".into(), json!("结果"));
        row.ai = Some([("f5".to_string(), AiCellMeta::ok(input.clone(), 1, None))].into_iter().collect());
        assert_eq!(cell_state(&target, &fields, &row, false), AiCellState::Ready);
        row.values.insert("f1".into(), json!("乙"));
        assert_eq!(cell_state(&target, &fields, &row, false), AiCellState::Stale);
        row.ai = Some([("f5".to_string(), AiCellMeta::failed(input, 2, None, "超时".into()))]
            .into_iter()
            .collect());
        assert_eq!(cell_state(&target, &fields, &row, false), AiCellState::Failed("超时".into()));
    }
    #[test]
    fn fingerprint_is_stable_when_field_is_renamed() {
        let mut fields = vec![field("f1", "画面", "text")];
        let target = ai_field("#画面");
        let data = row(&[("f1", json!("甲"))]);
        let before = fingerprint(&target, &fields, &data).expect("指纹");
        // 改名 + 模板同步改写 = 同一输入语义
        let renamed = ai_field("#画面描述");
        fields[0].name = "画面描述".into();
        assert_eq!(before, fingerprint(&renamed, &fields, &data).expect("指纹"));
    }
}
