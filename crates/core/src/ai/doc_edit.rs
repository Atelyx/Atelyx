//! AI 领域文档（表格 / 画布）编辑的公共步骤：读真源 → 算补丁 → 写回 → 结算回执。
//! 表与画布只提供模型差异（trait 实现），补丁合并、写盘与回执口径只此一处。

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use serde::Serialize;

use crate::history::{self, HistoryKind};
use crate::space_client::{SpaceClient, SpaceError};
use crate::vault::{self, CanvasFile, TableFile, VaultState};
use crate::write_queue::WriteQueue;

/// 空间补丁上行（表 / 画布各自的端点）。
pub type PatchFuture<'a> = Pin<Box<dyn Future<Output = Result<String, SpaceError>> + Send + 'a>>;

/// 领域文档模型：表与画布的差异面（其余步骤共用本模块实现）。
pub trait DocModel: Clone + Serialize + Send + Sync + 'static {
    type Ops: Send + Sync + 'static;
    type Patch: Clone + Send + Sync + 'static;

    /// 应用一批操作，返回受影响实体 id（整批成功才算一个补丁）。
    fn apply_ops(model: &mut Self, ops: &Self::Ops) -> Result<Vec<String>, String>;
    /// 操作条数（回执文案里的「N 项操作」）。
    fn ops_count(ops: &Self::Ops) -> usize;
    /// 与基线按稳定 id 做值 diff；无变化返回 None。
    fn diff(baseline: &Self, current: &Self) -> Option<Self::Patch>;
    /// 补丁涉及的实体 id（回执告诉模型后续可操作的 id）；无实体概念的文档返回空。
    fn touched_ids(_patch: &Self::Patch) -> Vec<String> {
        Vec::new()
    }
    /// 按稳定 id 合并补丁（外部改动落在未触及实体上时保留）。
    fn merge_patch(model: &mut Self, patch: &Self::Patch);
    fn set_title(&mut self, title: &str);
    fn set_updated_at(&mut self, ts: i64);
    fn id(&self) -> &str;
    /// 正文是否仍等于冻结快照（时间戳不参与；各文档按自己承载正文的字段比较）。
    fn same_content(&self, other: &Self) -> bool;
    fn kind() -> HistoryKind;
    /// 用户可见文档名（「表格」/「画布」）：回执文案共用。
    fn label() -> &'static str;
    /// 空间侧文档标识：历史记录 kind 与在场视图 id 共用。
    fn space_kind() -> &'static str;
    fn read_file(path: &Path) -> Result<Self, String>;
    fn write_file(path: &Path, model: &Self) -> Result<(), String>;
    /// 落盘后回写会话缓存（下次打开直接命中磁盘真源）。
    fn write_cache(state: &VaultState, file: &Path, rel: &str, model: &Self);
    fn parse(text: &str) -> Result<Self, String>;
    fn new_model(title: &str) -> Self;
    fn read_cached(state: &VaultState, root: &Path, rel: &str) -> Result<(PathBuf, Self), String>;
    /// 空间补丁端点（回执 = 落地后的仓库相对路径）。
    fn submit_patch<'a>(
        client: &'a SpaceClient,
        space_id: &'a str,
        rel: &'a str,
        patch: &'a Self::Patch,
    ) -> PatchFuture<'a>;
}

/// 一次编辑的计划：操作后模型、补丁与摘要。
pub struct EditPlan<M: DocModel> {
    pub after: M,
    pub patch: M::Patch,
    pub summary: String,
}

/// 一次编辑的落地结果：补丁落地后的仓库相对路径（服务端可能按标题改名）、操作后模型、
/// 实际上行补丁与摘要。
pub struct EditOutcome<M: DocModel> {
    pub file: String,
    pub after: M,
    pub patch: M::Patch,
    pub summary: String,
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
    M::apply_ops(&mut after, ops).map_err(PlanError::Ops)?;
    let Some(patch) = M::diff(baseline, &after) else {
        return Err(PlanError::Unchanged);
    };
    let mut stamped = after.clone();
    M::set_title(&mut stamped, title);
    M::set_updated_at(&mut stamped, updated_at);
    let summary = history::version_summary(M::kind(), &json(baseline), &json(&stamped));
    Ok(EditPlan { after: stamped, patch, summary })
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
///
/// 磁盘缺失 / 被外部替换为其他实体 = 按本端整份重建，与人工保存同一口径；
/// 读盘失败（权限 / 占用 / 损坏）如实报错——不拿内存模型覆盖一个仍然存在的文件。
/// 父目录在队列内创建：与写盘同一执行序，作业失败不留空目录。
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
    let root = root.to_path_buf();
    let rel = rel.to_string();
    let expected_id = expected_id.to_string();
    let after = after.clone();
    let patch = patch.clone();
    let title = title.to_string();
    queue
        .bulk(move || {
            // 建父目录随写盘一并执行：新建表格 / 画布的落点目录可能还不存在
            let path = vault::safe_join(&root, &rel, true)?;
            let mut disk = match M::read_file(&path) {
                Ok(model) if model.id() == expected_id => model,
                Err(_) if !path.is_file() => after,
                // 文件仍在盘上却读不出：不覆盖，把原因交回调用方
                Err(e) => return Err(format!("读取失败：{e}")),
                Ok(_) => after,
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
        Err(SpaceError::NotFound) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

/// 空间新建空模型（补丁端点要求目标已存在）。只新建：目标已存在即 `Conflict`，不覆盖。
/// 落盘的模型必须与随后上行的补丁同源（补丁 id 即该模型 id），否则服务端按串文件拒绝。
async fn create_space<M: DocModel>(
    client: &SpaceClient,
    space_id: &str,
    rel: &str,
    model: &M,
) -> Result<(), SpaceError> {
    let json = serde_json::to_string_pretty(model).map_err(|e| SpaceError::Network(e.to_string()))?;
    client.create_file_text(space_id, rel, &json).await
}

/// 补丁端点错误 → 模型可读原因（400 / 409 = 补丁与文件不匹配，须刷新重算）。
pub fn patch_error(e: SpaceError, what: &str) -> String {
    match e {
        SpaceError::BadRequest(m) | SpaceError::Conflict(m) => {
            format!("{what}已被他人修改，请刷新后重试（{m}）")
        }
        e => e.to_string(),
    }
}

/// 成功回执：操作数、摘要，以及补丁携带的实体 id（有实体概念的文档才列）。
pub fn edited_receipt<M: DocModel>(
    path: &str,
    ops: usize,
    summary: &str,
    patch: &M::Patch,
) -> String {
    let ids = M::touched_ids(patch);
    if ids.is_empty() {
        return format!("已编辑 {path}（{ops} 项操作：{summary}）");
    }
    format!("已编辑 {path}（{ops} 项操作：{summary}）；实体：{}", ids.join("、"))
}

/// 操作与现值相同：不写盘、不记版本，回执如实说明。
pub fn unchanged_receipt<M: DocModel>(path: &str) -> String {
    format!("{}未变化：{path}（操作与现值相同）", M::label())
}

/// 读不到本地真源（不存在之外的原因：损坏 / 权限 / schema 不符）。
pub fn unreadable_receipt<M: DocModel>(path: &str, reason: &str) -> String {
    format!("{}不存在或不可读：{path}（{reason}）", M::label())
}

/// 写盘 / 补丁上行的失败回执（界面提示与工具回执共用前缀）。
pub fn write_failed(reason: &str) -> String {
    format!("写入失败：{reason}")
}

/// 回执是否仍属当前打开的文件：空间补丁可能按标题改名，旧路径与新路径都算命中。
pub fn receipt_file_matches(current: Option<&str>, file: &str, new_file: &str) -> bool {
    current.is_some_and(|f| f == file || f == new_file)
}

/// 空间落盘：读真源 → 计划 → 补丁上行（服务端按稳定 id 仲裁）。
///
/// 读到不存在时按空模型起算并「只新建」落一个空模型，再上行补丁；建与补丁之间目标被他人
/// 建出 / 删除（`Conflict` / `NotFound`）时重读重算一次，不整份覆盖他人内容。
/// 操作未产生变化返回 `None`（不写盘、不记版本）。
pub async fn apply_space<M: DocModel>(
    client: &SpaceClient,
    space_id: &str,
    rel: &str,
    title: &str,
    updated_at: i64,
    ops: &M::Ops,
    what: &str,
) -> Result<Option<EditOutcome<M>>, String> {
    // 一次重试足够：重试读到的真源已是并发方落地后的内容，再冲突属持续争用，如实报错
    for attempt in 0..2 {
        let base = match read_space::<M>(client, space_id, rel).await? {
            Some(model) => model,
            None => {
                let fresh = M::new_model(title);
                let planned = match plan(&fresh, &fresh, ops, title, updated_at) {
                    Ok(planned) => planned,
                    Err(PlanError::Unchanged) => return Ok(None),
                    Err(PlanError::Ops(e)) => return Err(e),
                };
                match create_space::<M>(client, space_id, rel, &fresh).await {
                    Ok(()) => {}
                    // 读到不存在之后被他人建出：重读后按补丁上行
                    Err(SpaceError::Conflict(_)) if attempt == 0 => continue,
                    // 第二次仍冲突：如实回冲突原因，不把「被他人修改」说成新建失败
                    Err(SpaceError::Conflict(_)) => {
                        return Err(format!("{what}正在被他人修改，请稍后重试"))
                    }
                    Err(e) => return Err(format!("新建失败：{e}")),
                }
                let file = M::submit_patch(client, space_id, rel, &planned.patch)
                    .await
                    .map_err(|e| patch_error(e, what))?;
                return Ok(Some(EditOutcome {
                    file,
                    after: planned.after,
                    patch: planned.patch,
                    summary: planned.summary,
                }));
            }
        };
        let planned = match plan(&base, &base, ops, title, updated_at) {
            Ok(planned) => planned,
            Err(PlanError::Unchanged) => return Ok(None),
            Err(PlanError::Ops(e)) => return Err(e),
        };
        match M::submit_patch(client, space_id, rel, &planned.patch).await {
            Ok(file) => {
                return Ok(Some(EditOutcome {
                    file,
                    after: planned.after,
                    patch: planned.patch,
                    summary: planned.summary,
                }))
            }
            // 读到存在之后被删除：重读后按新建处理（补丁端点要求目标已存在）
            Err(SpaceError::NotFound) if attempt == 0 => continue,
            // 第二次仍被删：如实回冲突原因，不把「被他人修改」说成补丁非法
            Err(SpaceError::NotFound) => {
                return Err(format!("{what}正在被他人修改，请稍后重试"))
            }
            Err(e) => return Err(patch_error(e, what)),
        }
    }
    Err(format!("{what}正在被他人修改，请稍后重试"))
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

    fn ops_count(ops: &Self::Ops) -> usize {
        ops.len()
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

    /// 表格正文 = 字段 + 行；标题随文件名走，不参与落盘判定。
    fn same_content(&self, other: &Self) -> bool {
        self.fields == other.fields && self.rows == other.rows
    }

    fn kind() -> HistoryKind {
        HistoryKind::Table
    }

    fn label() -> &'static str {
        "表格"
    }

    fn space_kind() -> &'static str {
        "table"
    }

    fn read_file(path: &Path) -> Result<Self, String> {
        vault::read_table_file(path)
    }

    fn write_file(path: &Path, model: &Self) -> Result<(), String> {
        vault::write_table_file(path, model)
    }

    fn write_cache(state: &VaultState, file: &Path, rel: &str, model: &Self) {
        vault::cache_put_table(state, file, rel, model)
    }

    fn parse(text: &str) -> Result<Self, String> {
        let model: TableFile = serde_json::from_str(text).map_err(|e| format!("表格解析失败：{e}"))?;
        if model.schema != vault::TABLE_SCHEMA {
            return Err(format!("表格 schema 不匹配：{}", model.schema));
        }
        Ok(model)
    }

    fn new_model(title: &str) -> Self {
        vault::TableFile::new_file(title)
    }

    fn read_cached(state: &VaultState, root: &Path, rel: &str) -> Result<(PathBuf, Self), String> {
        vault::read_table_file_cached(state, root, rel)
    }

    fn submit_patch<'a>(
        client: &'a SpaceClient,
        space_id: &'a str,
        rel: &'a str,
        patch: &'a Self::Patch,
    ) -> PatchFuture<'a> {
        Box::pin(async move { client.table_patch(space_id, rel, patch).await.map(|ack| ack.file) })
    }
}

impl DocModel for CanvasFile {
    type Ops = Vec<crate::ai::edit_host::CanvasOp>;
    type Patch = vault::CanvasPatch;

    fn apply_ops(model: &mut Self, ops: &Self::Ops) -> Result<Vec<String>, String> {
        crate::ai::doc_ops::apply_canvas_ops(model, ops)
    }

    fn ops_count(ops: &Self::Ops) -> usize {
        ops.len()
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

    /// 画布正文 = 标题 + 节点 + 连线；标题参与落盘判定（改名经补丁上行）。
    fn same_content(&self, other: &Self) -> bool {
        self.title == other.title && self.nodes == other.nodes && self.edges == other.edges
    }

    fn kind() -> HistoryKind {
        HistoryKind::Canvas
    }

    fn label() -> &'static str {
        "画布"
    }

    fn space_kind() -> &'static str {
        "canvas"
    }

    /// 补丁新增 / 修改的节点与连线，以及删除的节点（按补丁顺序，去连续重复）。
    fn touched_ids(patch: &Self::Patch) -> Vec<String> {
        let mut ids: Vec<String> = patch.upsert_nodes.iter().map(|n| n.id.clone()).collect();
        ids.extend(patch.upsert_edges.iter().map(|e| e.id.clone()));
        ids.extend(patch.removed_node_ids.iter().cloned());
        ids.dedup();
        ids
    }

    fn read_file(path: &Path) -> Result<Self, String> {
        vault::read_canvas_file(path)
    }

    fn write_file(path: &Path, model: &Self) -> Result<(), String> {
        vault::write_canvas_file(path, model)
    }

    fn write_cache(state: &VaultState, file: &Path, rel: &str, model: &Self) {
        vault::cache_put_canvas(state, file, rel, model)
    }

    fn parse(text: &str) -> Result<Self, String> {
        let model: CanvasFile = serde_json::from_str(text).map_err(|e| format!("画布解析失败：{e}"))?;
        if model.schema != vault::CANVAS_SCHEMA {
            return Err(format!("画布 schema 不匹配：{}", model.schema));
        }
        Ok(model)
    }

    fn new_model(title: &str) -> Self {
        vault::CanvasFile::new_file(title)
    }

    fn read_cached(state: &VaultState, root: &Path, rel: &str) -> Result<(PathBuf, Self), String> {
        vault::read_canvas_file_cached(state, root, rel)
    }

    fn submit_patch<'a>(
        client: &'a SpaceClient,
        space_id: &'a str,
        rel: &'a str,
        patch: &'a Self::Patch,
    ) -> PatchFuture<'a> {
        Box::pin(async move { client.canvas_patch(space_id, rel, patch).await.map(|ack| ack.file) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::edit_host::{CanvasOp, TableOp};
    use crate::history::RecordOpts;
    use serde_json::json;

    fn table() -> TableFile {
        let mut model = vault::TableFile::new_file("任务");
        model.rows.push(vault::TableRow {
            id: "r1".into(),
            values: Default::default(),
            height: None,
            styles: None,
            ai: None,
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
        assert_eq!(planned.patch.upsert_rows.len(), 1, "补丁只带受影响行");
        assert!(!planned.summary.is_empty());
        // 操作与现值相同 = 未变化（不写盘）
        let same = vec![TableOp::SetCell {
            row_id: "r1".into(),
            field_id: baseline.fields[0].id.clone(),
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
    fn plan_canvas_patch_carries_new_node() {
        let baseline = vault::CanvasFile::new_file("画");
        let ops = vec![CanvasOp::AddTextCard { text: "卡".into(), x: Some(16.0), y: Some(16.0) }];
        let planned = plan(&baseline, &baseline, &ops, "画", 1).expect("应产出计划");
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
            ai: None,
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

    #[test]
    fn commit_local_creates_parent_and_refuses_unreadable_file() {
        let dir = crate::vault::test_support::TempDir::new("doc-edit-commit-guard");
        let root = dir.to_path_buf();
        let queue = WriteQueue::new();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let baseline = table();
        let ops = vec![TableOp::AddRow { cells: vec![] }];
        let planned = plan(&baseline, &baseline, &ops, "排期", 1).expect("应产出计划");
        // 落点目录不存在：父目录随写盘一并创建
        runtime
            .block_on(commit_local(
                &queue,
                &root,
                "计划/排期.atb",
                &baseline.id,
                &planned.after,
                &planned.patch,
                "排期",
                1,
            ))
            .expect("新建应成功");
        assert!(root.join("计划/排期.atb").is_file());
        // 文件仍在盘上却读不出（损坏）：不覆盖，报错带出原因
        std::fs::write(root.join("坏.atb"), "{不是 JSON").unwrap();
        let err = runtime
            .block_on(commit_local(
                &queue,
                &root,
                "坏.atb",
                &baseline.id,
                &planned.after,
                &planned.patch,
                "坏",
                1,
            ))
            .expect_err("读不出的文件不得被覆盖");
        assert!(err.contains("读取失败"), "{err}");
        assert_eq!(std::fs::read_to_string(root.join("坏.atb")).unwrap(), "{不是 JSON");
    }

    /// 回执文案是工具侧与界面共用的对外文本：改动需同步壳侧断言。
    #[test]
    fn receipts_keep_existing_text() {
        let baseline = table();
        let ops = vec![TableOp::AddRow { cells: vec![] }];
        let planned = plan(&baseline, &baseline, &ops, "任务", 1).expect("应产出计划");
        assert_eq!(
            edited_receipt::<TableFile>("任务.atb", 1, &planned.summary, &planned.patch),
            format!("已编辑 任务.atb（1 项操作：{}）", planned.summary)
        );
        assert_eq!(
            unchanged_receipt::<TableFile>("任务.atb"),
            "表格未变化：任务.atb（操作与现值相同）"
        );
        assert_eq!(
            unreadable_receipt::<TableFile>("任务.atb", "坏"),
            "表格不存在或不可读：任务.atb（坏）"
        );

        let canvas = vault::CanvasFile::new_file("画");
        let ops = vec![CanvasOp::AddTextCard { text: "卡".into(), x: Some(1.0), y: Some(1.0) }];
        let planned = plan(&canvas, &canvas, &ops, "画", 1).expect("应产出计划");
        let ids = planned.patch.upsert_nodes.iter().map(|n| n.id.clone()).collect::<Vec<_>>().join("、");
        assert_eq!(
            edited_receipt::<CanvasFile>("画.atlx", 1, &planned.summary, &planned.patch),
            format!("已编辑 画.atlx（1 项操作：{}）；实体：{ids}", planned.summary)
        );
        assert_eq!(
            unchanged_receipt::<CanvasFile>("画.atlx"),
            "画布未变化：画.atlx（操作与现值相同）"
        );
        assert_eq!(
            unreadable_receipt::<CanvasFile>("画.atlx", "坏"),
            "画布不存在或不可读：画.atlx（坏）"
        );
        assert_eq!(write_failed("磁盘满"), "写入失败：磁盘满");
    }

    /// 落盘判定只看正文：时间戳不参与；表格标题随文件名走不参与，画布标题经补丁上行参与。
    #[test]
    fn same_content_ignores_stamps_and_compares_own_body() {
        let table = table();
        let mut stamped = table.clone();
        stamped.updated_at = 9;
        stamped.title = "改名".into();
        assert!(table.same_content(&stamped), "表格标题与时间戳不影响落盘判定");
        stamped.rows[0].values.insert(table.fields[0].id.clone(), json!("x"));
        assert!(!table.same_content(&stamped), "单元格变化即未落定");

        let canvas = vault::CanvasFile::new_file("画");
        let mut stamped = canvas.clone();
        stamped.updated_at = 9;
        assert!(canvas.same_content(&stamped), "时间戳不参与");
        stamped.title = "改名".into();
        assert!(!canvas.same_content(&stamped), "画布标题经补丁上行，参与判定");
        let ops = vec![CanvasOp::AddTextCard { text: "卡".into(), x: None, y: None }];
        let planned = plan(&canvas, &canvas, &ops, "画", 1).expect("应产出计划");
        assert!(!canvas.same_content(&planned.after), "新增节点即未落定");
    }

    /// 回执归属：世代 + 文件路径（含标题漂移后的新路径）双校验，切文件 / 关面即作废。
    #[test]
    fn receipt_belongs_to_currently_open_file() {
        assert!(receipt_file_matches(Some("表.atb"), "表.atb", "表.atb"));
        assert!(receipt_file_matches(Some("新.atb"), "旧.atb", "新.atb"), "标题漂移后的新路径仍命中");
        assert!(!receipt_file_matches(Some("别的.atb"), "旧.atb", "新.atb"), "切到别的文件即作废");
        assert!(!receipt_file_matches(None, "旧.atb", "新.atb"), "会话已关闭不命中");
    }

    /// 本地矩阵：打开 / 未打开决定基线来源；成功落盘后由冻结快照与正文是否一致决定脏态与基线推进。
    #[test]
    fn local_matrix_open_state_decides_baseline_and_settle() {
        let dir = crate::vault::test_support::TempDir::new("doc-edit-matrix");
        let root = dir.to_path_buf();
        let queue = WriteQueue::new();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let baseline = table();
        crate::vault::write_table_file(&root.join("任务.atb"), &baseline).unwrap();
        let author = crate::history::HistoryAuthor {
            id: "u1".into(),
            name: "作者".into(),
            device: "dev".into(),
        };

        // 已打开：基线 = 上次落盘，模型 = 含用户未保存编辑；补丁覆盖两者，一次落盘
        let mut opened = baseline.clone();
        opened.rows[0].values.insert(baseline.fields[0].id.clone(), json!("用户改的"));
        let ops = vec![TableOp::AddRow { cells: vec![] }];
        let planned = plan(&baseline, &opened, &ops, "任务", 7).expect("应产出计划");
        let written = runtime
            .block_on(commit_local(
                &queue,
                &root,
                "任务.atb",
                &baseline.id,
                &planned.after,
                &planned.patch,
                "任务",
                7,
            ))
            .expect("落盘成功");
        let disk = crate::vault::read_table_file(&root.join("任务.atb")).unwrap();
        assert_eq!(disk.rows.len(), 2, "用户编辑行 + AI 新增行都落盘");
        assert!(disk.same_content(&written), "回执冻结快照与磁盘正文一致");
        assert!(written.same_content(&planned.after), "无并发编辑：回执结算清脏并推进基线");

        // 写盘在途用户又编辑：正文已不等于冻结快照，回执不得清脏 / 推进基线
        let mut later = planned.after.clone();
        later.rows.push(vault::TableRow {
            id: "后续".into(),
            values: Default::default(),
            height: None,
            styles: None,
            ai: None,
        });
        assert!(!written.same_content(&later), "在途新编辑保持脏");

        // 历史内容 = 落盘后的模型 JSON；同一内容重复记录不产生新版本
        let content = json(&written);
        for opts in [
            RecordOpts::agent_edit(content.clone(), author.clone(), None),
            RecordOpts::edit(content.clone(), author.clone()),
        ] {
            crate::history::record_version(&root, HistoryKind::Table, "任务.atb", &opts).unwrap();
        }
        let versions = crate::history::load_history(&root, HistoryKind::Table, "任务.atb");
        assert_eq!(versions.len(), 1, "同内容只记一版");
        assert_eq!(versions[0].content, content, "历史正文即落盘正文");

        // 未打开：以磁盘真源为源，不引入任何会话内编辑
        let from_disk = read_local_or_new::<TableFile>(&VaultState::default(), &root, "任务.atb", "任务")
            .expect("应读到磁盘真源");
        assert_eq!(from_disk.rows.len(), 2);
        assert!(from_disk.same_content(&disk));

        // 磁盘被换成另一个实体（id 不符）：按本端整份重建，不与陌生模型做 id 合并
        let foreign = vault::TableFile::new_file("别的");
        crate::vault::write_table_file(&root.join("任务.atb"), &foreign).unwrap();
        let rebuilt = runtime
            .block_on(commit_local(
                &queue,
                &root,
                "任务.atb",
                &baseline.id,
                &planned.after,
                &planned.patch,
                "任务",
                8,
            ))
            .expect("整份重建应成功");
        let disk = crate::vault::read_table_file(&root.join("任务.atb")).unwrap();
        assert_eq!(disk.id, baseline.id, "落点回到本端实体");
        assert!(disk.same_content(&rebuilt));
        assert_eq!(disk.rows.len(), 2, "陌生实体的内容不带入");
    }
}
