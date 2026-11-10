//! AI 领域文档（表格 / 画布）编辑的公共步骤：读真源 → 算补丁 → 写回 → 结算回执。
//! 表与画布只提供模型差异（trait 实现），补丁合并、写盘与回执口径只此一处。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde::Serialize;

use crate::history::{self, HistoryKind};
use crate::space_client::SpaceClient;
use crate::vault::{self, CanvasFile, TableFile, VaultState};
use crate::write_queue::WriteQueue;

/// 空间补丁上行（表 / 画布各自的端点）。
pub type PatchFuture<'a> = Pin<Box<dyn Future<Output = Result<String, String>> + Send + 'a>>;

/// 领域文档模型：表与画布的差异面（其余步骤共用本模块实现）。
pub trait DocModel: Clone + Serialize + Send + 'static {
    type Ops;
    type Patch: Clone + Send + 'static;

    /// 应用一批操作，返回受影响实体 id（整批成功才算一个补丁）。
    fn apply_ops(model: &mut Self, ops: &Self::Ops) -> Result<Vec<String>, String>;
    /// 与基线按稳定 id 做值 diff；无变化返回 None。
    fn diff(baseline: &Self, current: &Self) -> Option<Self::Patch>;
    /// 按稳定 id 合并补丁（外部改动落在未触及实体上时保留）。
    fn merge_patch(model: &mut Self, patch: &Self::Patch);
    fn set_title(&mut self, title: &str);
    fn set_updated_at(&mut self, ts: i64);
    fn id(&self) -> &str;
    fn kind() -> HistoryKind;
    fn read_file(path: &Path) -> Result<Self, String>;
    fn write_file(path: &Path, model: &Self) -> Result<(), String>;
    fn parse(text: &str) -> Result<Self, String>;
    fn new_model(title: &str) -> Self;
    fn read_cached(state: &VaultState, root: &Path, rel: &str) -> Result<(PathBuf, Self), String>;
    fn cache_put(state: &VaultState, path: &Path, rel: &str, model: &Self);
    /// 空间补丁端点（回执 = 落地后的仓库相对路径）。
    fn submit_patch<'a>(
        client: &'a SpaceClient,
        space_id: &'a str,
        rel: &'a str,
        patch: &'a Self::Patch,
    ) -> PatchFuture<'a>;
}

/// 一次编辑的中间结果：基线、操作后模型、补丁、摘要与受影响实体。
pub struct EditPlan<M: DocModel> {
    pub baseline: M,
    pub after: M,
    pub patch: M::Patch,
    pub summary: String,
    pub touched: Vec<String>,
}

/// 计划失败原因：调用方按自己的文档类型给可读文案。
#[derive(Debug)]
pub enum PlanError {
    /// 操作未产生任何变化（不写盘、不记版本）。
    Unchanged,
    /// 操作本身失败（id 不存在 / 值类型不符等）。
    Ops(String),
}

/// 在「用户眼前模型」上应用操作并与保存基线 diff：打开会话时基线 = 上次落盘、模型 = 含未保存编辑。
/// 成功时把标题与时间戳盖在操作后模型上（补丁不携带标题，文件名才是真源）。
pub fn plan<M: DocModel>(
    baseline: &M,
    base: &M,
    ops: &M::Ops,
    title: &str,
    updated_at: i64,
) -> Result<EditPlan<M>, PlanError> {
    let mut after = base.clone();
    let touched = M::apply_ops(&mut after, ops).map_err(PlanError::Ops)?;
    let Some(patch) = M::diff(baseline, &after) else {
        return Err(PlanError::Unchanged);
    };
    let mut stamped = after.clone();
    M::set_title(&mut stamped, title);
    M::set_updated_at(&mut stamped, updated_at);
    let summary = history::version_summary(M::kind(), &json(baseline), &json(&stamped));
    Ok(EditPlan { baseline: baseline.clone(), after: stamped, patch, summary, touched })
}

/// 读本地真源；文件不存在 = 按标题新建空模型（坏文件仍报错，不得当空模型覆盖）。
pub fn read_local_or_new<M: DocModel>(
    state: &VaultState,
    root: &Path,
    rel: &str,
    title: &str,
) -> Result<M, String> {
    match M::read_cached(state, root, rel) {
        Ok((_, model)) => Ok(model),
        Err(_) if !vault::file_exists(root, rel).unwrap_or(false) => Ok(M::new_model(title)),
        Err(e) => Err(e),
    }
}

/// 本地落盘：在写盘队列内重读磁盘、按稳定 id 合并补丁后原子写。
/// 磁盘实体 id 与期望不符（被外部替换 / 缺失）= 按本端整份重建，与人工保存同一口径。
pub async fn commit_local<M: DocModel>(
    queue: &WriteQueue,
    root: &Path,
    rel: &str,
    expected_id: &str,
    after: &M,
    patch: &M::Patch,
    title: &str,
    updated_at: i64,
) -> Result<M, String> {
    let path = vault::safe_join(root, rel, false)?;
    let expected_id = expected_id.to_string();
    let after = after.clone();
    let patch = patch.clone();
    let title = title.to_string();
    queue
        .bulk(move || {
            let mut disk = match M::read_file(&path) {
                Ok(model) if model.id() == expected_id => model,
                _ => after,
            };
            M::merge_patch(&mut disk, &patch);
            disk.set_title(&title);
            disk.set_updated_at(updated_at);
            M::write_file(&path, &disk)?;
            Ok(disk)
        })
        .await
}

/// 读空间真源：文件不存在返回 None（调用方按新建处理），其余错误如实上抛。
pub async fn read_space<M: DocModel>(
    client: &SpaceClient,
    space_id: &str,
    rel: &str,
) -> Result<Option<M>, String> {
    match client.get_file_text(space_id, rel).await {
        Ok(text) => M::parse(&text).map(Some),
        Err(crate::space_client::SpaceError::NotFound) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// 空间新建：先把空模型整份上行（补丁端点要求目标已存在），返回该模型作为后续操作的基线。
pub async fn create_space<M: DocModel>(
    client: &SpaceClient,
    space_id: &str,
    rel: &str,
    title: &str,
) -> Result<M, String> {
    let model = M::new_model(title);
    let json = serde_json::to_string_pretty(&model).map_err(|e| e.to_string())?;
    client
        .put_file_text(space_id, rel, &json)
        .await
        .map_err(|e| format!("新建失败：{e}"))?;
    Ok(model)
}

/// 空间落盘：按一次补丁上行（服务端按稳定 id 仲裁），返回落地后的仓库相对路径。
pub async fn commit_space<M: DocModel>(
    client: &SpaceClient,
    space_id: &str,
    rel: &str,
    patch: &M::Patch,
) -> Result<String, String> {
    M::submit_patch(client, space_id, rel, patch).await
}

/// 领域文件显示标题：末段去扩展名（新建表格 / 画布的标题口径与壳一致）。
pub fn file_label(rel: &str) -> String {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    vault::file_stem(name).to_string()
}

fn json<T: Serialize>(model: &T) -> String {
    serde_json::to_string(model).unwrap_or_default()
}

impl DocModel for TableFile {
    type Ops = Vec<crate::ai::edit_host::TableOp>;
    type Patch = vault::TablePatch;

    fn apply_ops(model: &mut Self, ops: &Self::Ops) -> Result<Vec<String>, String> {
        crate::ai::doc_ops::apply_table_ops(model, ops)
    }

    fn diff(baseline: &Self, current: &Self) -> Option<Self::Patch> {
        vault::compute_table_patch(baseline, current)
    }

    fn merge_patch(model: &mut Self, patch: &Self::Patch) {
        vault::apply_table_patch(model, patch);
    }

    fn set_title(&mut self, title: &str) {
        self.title = title.to_string();
    }

    fn set_updated_at(&mut self, ts: i64) {
        self.updated_at = ts;
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn kind() -> HistoryKind {
        HistoryKind::Table
    }

    fn read_file(path: &Path) -> Result<Self, String> {
        vault::read_table_file(path)
    }

    fn write_file(path: &Path, model: &Self) -> Result<(), String> {
        vault::write_table_file(path, model)
    }

    fn parse(text: &str) -> Result<Self, String> {
        let model: TableFile = serde_json::from_str(text).map_err(|e| format!("表格解析失败：{e}"))?;
        if model.schema != vault::TABLE_SCHEMA {
            return Err(format!("表格 schema 不匹配：{}", model.schema));
        }
        Ok(model)
    }

    fn new_model(title: &str) -> Self {
        crate::table_ops::new_table_file(title)
    }

    fn read_cached(state: &VaultState, root: &Path, rel: &str) -> Result<(PathBuf, Self), String> {
        vault::read_table_file_cached(state, root, rel)
    }

    fn cache_put(state: &VaultState, path: &Path, rel: &str, model: &Self) {
        vault::cache_put_table(state, path, rel, model);
    }

    fn submit_patch<'a>(
        client: &'a SpaceClient,
        space_id: &'a str,
        rel: &'a str,
        patch: &'a Self::Patch,
    ) -> PatchFuture<'a> {
        Box::pin(async move {
            match client.table_patch(space_id, rel, patch).await {
                Ok(ack) => Ok(ack.file),
                Err(crate::space_client::SpaceError::BadRequest(m)) => {
                    Err(format!("表格已被他人修改，请刷新后重试（{m}）"))
                }
                Err(e) => Err(e.to_string()),
            }
        })
    }
}

impl DocModel for CanvasFile {
    type Ops = Vec<crate::ai::edit_host::CanvasOp>;
    type Patch = vault::CanvasPatch;

    fn apply_ops(model: &mut Self, ops: &Self::Ops) -> Result<Vec<String>, String> {
        crate::ai::doc_ops::apply_canvas_ops(model, ops)
    }

    fn diff(baseline: &Self, current: &Self) -> Option<Self::Patch> {
        vault::compute_canvas_patch(baseline, current)
    }

    fn merge_patch(model: &mut Self, patch: &Self::Patch) {
        vault::apply_canvas_patch(model, patch);
    }

    fn set_title(&mut self, title: &str) {
        self.title = title.to_string();
    }

    fn set_updated_at(&mut self, ts: i64) {
        self.updated_at = ts;
    }

    fn id(&self) -> &str {
        &self.id
    }

    fn kind() -> HistoryKind {
        HistoryKind::Canvas
    }

    fn read_file(path: &Path) -> Result<Self, String> {
        vault::read_canvas_file(path)
    }

    fn write_file(path: &Path, model: &Self) -> Result<(), String> {
        vault::write_canvas_file(path, model)
    }

    fn parse(text: &str) -> Result<Self, String> {
        let model: CanvasFile = serde_json::from_str(text).map_err(|e| format!("画布解析失败：{e}"))?;
        if model.schema != vault::CANVAS_SCHEMA {
            return Err(format!("画布 schema 不匹配：{}", model.schema));
        }
        Ok(model)
    }

    fn new_model(title: &str) -> Self {
        crate::table_ops::new_canvas_file(title)
    }

    fn read_cached(state: &VaultState, root: &Path, rel: &str) -> Result<(PathBuf, Self), String> {
        vault::read_canvas_file_cached(state, root, rel)
    }

    fn cache_put(state: &VaultState, path: &Path, rel: &str, model: &Self) {
        vault::cache_put_canvas(state, path, rel, model);
    }

    fn submit_patch<'a>(
        client: &'a SpaceClient,
        space_id: &'a str,
        rel: &'a str,
        patch: &'a Self::Patch,
    ) -> PatchFuture<'a> {
        Box::pin(async move {
            match client.canvas_patch(space_id, rel, patch).await {
                Ok(ack) => Ok(ack.file),
                Err(crate::space_client::SpaceError::BadRequest(m)) => {
                    Err(format!("画布已被他人修改，请刷新后重试（{m}）"))
                }
                Err(e) => Err(e.to_string()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::edit_host::{CanvasOp, TableOp};
    use serde_json::json;

    fn table() -> TableFile {
        let mut model = crate::table_ops::new_table_file("任务");
        model.rows.push(vault::TableRow {
            id: "r1".into(),
            values: Default::default(),
            height: None,
            styles: None,
        });
        model
    }

    #[test]
    fn plan_stamps_title_and_reports_unchanged() {
        let baseline = table();
        let field = baseline.fields[0].id.clone();
        let ops = vec![TableOp::SetCell {
            row_id: "r1".into(),
            field_id: field,
            value: json!("写文档"),
        }];
        let planned = plan(&baseline, &baseline, &ops, "新名", 42).expect("应产出计划");
        assert_eq!(planned.after.title, "新名");
        assert_eq!(planned.after.updated_at, 42);
        assert_eq!(planned.touched, vec!["r1".to_string()]);
        assert!(!planned.summary.is_empty());
        // 操作与现值相同 = 未变化（不写盘）
        let same = vec![TableOp::SetCell {
            row_id: "r1".into(),
            field_id: planned.baseline.fields[0].id.clone(),
            value: json!("写文档"),
        }];
        assert!(matches!(
            plan(&planned.after, &planned.after, &same, "新名", 42),
            Err(PlanError::Unchanged)
        ));
        // 操作失败原因原样带出
        let bad = vec![TableOp::SetCell {
            row_id: "nope".into(),
            field_id: "f".into(),
            value: json!("x"),
        }];
        assert!(matches!(plan(&baseline, &baseline, &bad, "新名", 42), Err(PlanError::Ops(_))));
    }

    #[test]
    fn plan_keeps_user_edits_as_base_and_diffs_against_saved() {
        // 打开会话：base 含用户未保存编辑，baseline = 上次落盘 → 补丁包含用户编辑 + AI 操作
        let baseline = table();
        let mut base = baseline.clone();
        base.rows[0]
            .values
            .insert(baseline.fields[0].id.clone(), json!("用户改的"));
        let ops = vec![TableOp::AddRow { cells: vec![] }];
        let planned = plan(&baseline, &base, &ops, "任务", 1).expect("应产出计划");
        assert_eq!(planned.after.rows.len(), 2, "用户未保存的新增与 AI 操作一并落盘");
        assert_eq!(planned.patch.upsert_rows.len(), 2, "补丁覆盖用户改动与 AI 新增");
    }

    #[test]
    fn plan_canvas_reports_touched_entities() {
        let baseline = crate::table_ops::new_canvas_file("画");
        let ops = vec![CanvasOp::AddTextCard { text: "卡".into(), x: Some(16.0), y: Some(16.0) }];
        let planned = plan(&baseline, &baseline, &ops, "画", 1).expect("应产出计划");
        assert_eq!(planned.touched.len(), 1);
        assert_eq!(planned.patch.upsert_nodes.len(), 1);
    }

    #[test]
    fn commit_local_merges_into_disk_truth() {
        let dir = crate::vault::test_support::TempDir::new("doc-edit-commit");
        let root = dir.to_path_buf();
        let baseline = table();
        crate::vault::write_table_file(&root.join("表.atb"), &baseline).unwrap();
        let ops = vec![TableOp::AddRow { cells: vec![] }];
        let planned = plan(&baseline, &baseline, &ops, "表", 7).expect("应产出计划");
        // 磁盘在计划之后被外部改过（新增一行）：落盘时应保留外部改动
        let mut external = baseline.clone();
        external.rows.push(vault::TableRow {
            id: "外部".into(),
            values: Default::default(),
            height: None,
            styles: None,
        });
        crate::vault::write_table_file(&root.join("表.atb"), &external).unwrap();
        let queue = WriteQueue::new();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let written = runtime
            .block_on(commit_local(
                &queue,
                &root,
                "表.atb",
                &baseline.id,
                &planned.after,
                &planned.patch,
                "表",
                7,
            ))
            .expect("落盘成功");
        assert_eq!(written.rows.len(), 3, "外部新增 + AI 新增都保留");
        assert!(written.rows.iter().any(|r| r.id == "外部"));
        assert_eq!(written.updated_at, 7);
        let on_disk = crate::vault::read_table_file(&root.join("表.atb")).unwrap();
        assert_eq!(on_disk.rows.len(), 3);
    }

    #[test]
    fn read_local_or_new_distinguishes_missing_from_broken() {
        let dir = crate::vault::test_support::TempDir::new("doc-edit-read");
        let state = VaultState::default();
        let root = dir.to_path_buf();
        // 不存在 = 新建空模型（标题取文件名）
        let model = read_local_or_new::<TableFile>(&state, &root, "计划.atb", "计划").unwrap();
        assert_eq!(model.title, "计划");
        // 存在但解析失败 = 报错（不得当空模型覆盖）
        std::fs::write(root.join("坏.atb"), "{不是 JSON").unwrap();
        assert!(read_local_or_new::<TableFile>(&state, &root, "坏.atb", "坏").is_err());
    }
}
