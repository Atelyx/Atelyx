//! 笔记协作 yrs 文档绑定与基线对账：单活动文档的 CRDT 增量收发、基线标签提案与 awareness 光标状态。
//!
//! 文档以 `OffsetKind::Utf16` 创建：TextRef 的 insert/remove 索引与 clock 消耗均按 UTF-16 code unit
//! 计（与 Yjs 数据模型一致）；yrs 默认的 `OffsetKind::Bytes` 索引单位为 UTF-8 字节，不用于本模块。

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use yrs::updates::decoder::Decode;
use yrs::updates::encoder::{Encode, Encoder, EncoderV1};
use yrs::{
    ClientID, Doc, GetString, OffsetKind, Options, ReadTxn, StateVector, Text, Transact, Update,
};

use crate::merge3::{diff_hunks, merge3, ConflictSide};
use crate::space_frame::{
    baseline_id_of, byte_to_utf16_offset, decode_awareness_update, decode_note_payload,
    encode_awareness_update, encode_note_baseline, encode_note_sync, read_var_bytes, read_var_uint,
    write_var_bytes, write_var_uint, AwarenessEntry, DocTag, NoteFrame, Reader, SYNC_MSG_STEP1,
    SYNC_MSG_STEP2, SYNC_MSG_UPDATE,
};

/// 本地编辑事务的 origin 标记（可追溯；出站过滤只认 [`REMOTE_ORIGIN`]）。
const LOCAL_ORIGIN: &str = "note-collab-local";
/// 远端合入事务的 origin：文档更新订阅据此跳过回发（防回环）。
const REMOTE_ORIGIN: &str = "note-collab-remote";
/// 基线 seed 的固定 clientID：各端以同一正文重建基线时 struct 字节一致，合并幂等。
const SEED_CLIENT_ID: u64 = 1;
/// 异基线帧回通告的最小间隔（同一文件收敛窗口内防刷屏）。
const BASELINE_REPLY_INTERVAL: Duration = Duration::from_secs(2);
/// 远端 awareness 状态超时：期间无更新视为离线剔除。
const AWARENESS_TIMEOUT: Duration = Duration::from_secs(30);
/// awareness 保活重发间隔：空闲端（不再编辑/重连）的身份帧须按此周期重发，否则被对端超时剔除。
const AWARENESS_KEEPALIVE: Duration = Duration::from_secs(15);
/// 本端状态上行的最小间隔：拖选等连续变化按此合并，最新一份由 [`NoteCollab::flush_awareness`] 补发。
const AWARENESS_MIN_INTERVAL: Duration = Duration::from_millis(50);
/// awareness state 缺失或解析失败时的光标颜色兜底。
const FALLBACK_CURSOR_COLOR: &str = "#9aa0a6";

/// 打开文档的出站成果：两帧均已组好（含本端 tag），壳按序原样上行。
pub struct OpenOutcome {
    /// 基线通告帧（`note-sync` 通道载荷，`BASELINE` 类型）。
    pub baseline_frame: Vec<u8>,
    /// SyncStep1 握手帧（`note-sync` 通道载荷，`SYNC` 类型内嵌消息头 0）。
    pub sync_step1: Vec<u8>,
}

/// 入站帧的处理结果；`reply` 为应上行的完整帧（可能多帧，按序上行），空 = 无需回复。
#[derive(Default)]
pub struct RemoteOutcome {
    pub reply: Vec<Vec<u8>>,
    pub doc_changed: bool,
    /// `doc_changed` 时的合并后新全文（供壳刷新视图，省一次 `current_text()`）。
    pub text: Option<String>,
    /// Relocate 帧的（旧路径 → 新路径）：文档不动，壳负责改挂路径。
    pub relocated: Option<(String, String)>,
    /// 基线重建后本端 awareness 需重发的增量（新 clientID 的身份帧）。
    pub awareness: Option<Vec<u8>>,
}

/// 远端光标：awareness state JSON 的提取结果（缺失/解析失败的字段给空值兜底）。
pub struct RemoteCursor {
    pub client_id: u64,
    pub name: String,
    pub color: String,
    pub anchor: Option<u32>,
    pub head: Option<u32>,
}

struct RemoteState {
    clock: u64,
    state_json: String,
    seen: Instant,
    /// 上报该条目的连接（名册剔除该连接时一并移除，对端退出即时生效）。
    peer_id: String,
}

struct ActiveDoc {
    file: String,
    doc: Doc,
    ytext: yrs::TextRef,
    tag: DocTag,
    /// 当前文档基线的 seed 正文（`tag.id` 即其内容标识，兼容裁决据它复核）。
    baseline_text: String,
    /// 最近确认已落盘的正文：三方合并的共同祖先。重建不推进（未落盘内容不得记为已落盘）。
    last_flushed: String,
}

pub struct NoteCollab {
    author: String,
    /// 活动文档更新订阅推出的内嵌 Update 消息（observe 回调只在此落字，方法末尾统一取走）。
    outbox: Arc<Mutex<Vec<Vec<u8>>>>,
    doc: Option<ActiveDoc>,
    /// 各文件已知最大基线序号（含本端已发；提案取更高序）。量级 = 出现过的路径数，
    /// 不做淘汰：淘汰后提案序号可能回退引发额外一轮协商。
    room_seqs: HashMap<String, u64>,
    baseline_reply_at: HashMap<String, Instant>,
    /// 实证与本地基线正文不符的对端（键 = `peer_id\0file`）：其 sync 帧一律不应用，基线重建时解除。
    incompatible: HashSet<String>,
    // ---- awareness 本端与远端状态 ----
    local_user_json: Option<String>,
    local_selection: Option<(u32, u32)>,
    local_clock: u64,
    /// 本端 state JSON 上次广播值：变化才产出增量。
    local_state_json: Option<String>,
    /// 已上行的本端 state JSON：与 `local_state_json` 不同 = 有被限频压下的变化待补发。
    local_sent_json: Option<String>,
    /// 本端身份帧上次广播时刻（保活与限频共用记账）。
    awareness_sent_at: Option<Instant>,
    remote_states: HashMap<u64, RemoteState>,
}

impl NoteCollab {
    pub fn new(author: String) -> Self {
        NoteCollab {
            author,
            outbox: Arc::new(Mutex::new(Vec::new())),
            doc: None,
            room_seqs: HashMap::new(),
            baseline_reply_at: HashMap::new(),
            incompatible: HashSet::new(),
            local_user_json: None,
            local_selection: None,
            local_clock: 0,
            local_state_json: None,
            local_sent_json: None,
            awareness_sent_at: None,
            remote_states: HashMap::new(),
        }
    }

    /// 打开/重挂文件：以磁盘正文重建确定性 seed 基线（不广播），随后通告基线并握手索取对端状态。
    /// 若已有活动文档，旧文档被直接销毁（其离场 awareness 帧被丢弃）——壳层换文件路径须先显式
    /// [`close`](Self::close) 上行离场帧。
    pub fn open(&mut self, file: &str, text: &str) -> OpenOutcome {
        self.close();
        let (doc, ytext) = spawn_doc(text, &self.outbox);
        let tag = DocTag {
            seq: self.next_seq(file),
            author: self.author.clone(),
            id: baseline_id_of(text),
        };
        self.doc = Some(ActiveDoc {
            file: file.to_owned(),
            doc,
            ytext,
            tag: tag.clone(),
            baseline_text: text.to_owned(),
            last_flushed: text.to_owned(),
        });
        OpenOutcome {
            baseline_frame: encode_note_baseline(&tag, text, text),
            sync_step1: self
                .sync_step1()
                .expect("活动文档刚建立，sync_step1 必有帧"),
        }
    }

    /// 销毁活动文档。返回离场 awareness 帧（null 条目，对端收到后移除本端光标；
    /// 从未发过身份 = None）。序号记忆跨文档保留；该文件的实证黑名单与限频随路径身份一并作废。
    pub fn close(&mut self) -> Option<Vec<u8>> {
        let active = self.doc.take()?;
        let file = active.file;
        self.clear_incompatible(&file);
        self.baseline_reply_at.remove(&file);
        self.remote_states.clear();
        self.local_selection = None;
        self.local_state_json = None;
        self.local_sent_json = None;
        // null 帧用旧文档的 clientID（对端按它移除条目），clock 沿用本端 awareness 序列
        let leave = self.local_user_json.take().map(|_| {
            self.local_clock += 1;
            encode_awareness_update(&[AwarenessEntry {
                client_id: active.doc.client_id().get(),
                clock: self.local_clock,
                state_json: "null".to_string(),
            }])
        });
        self.local_user_json = None;
        self.local_clock = 0;
        // 更新订阅按命名挂在 doc 上，随 doc drop 一同消失
        leave
    }

    pub fn tag(&self) -> Option<&DocTag> {
        Some(&self.doc.as_ref()?.tag)
    }

    /// 文档正文真值（未打开 = None）。
    pub fn current_text(&self) -> Option<String> {
        let active = self.doc.as_ref()?;
        let txn = active.doc.transact();
        Some(active.ytext.get_string(&txn))
    }

    /// 本地编辑：以文档当前文本为基准求差并合入 ytext（差分基于文档真值，调用方无需
    /// 维护镜像——保存合流等导致文档落后于编辑器文本时，差分会把落盘内容一并补进文档）。
    /// 返回 = 需上行的内嵌 Update 消息（无变化 / 未打开 = None）。
    pub fn local_edit(&mut self, new: &str) -> Option<Vec<u8>> {
        let before = self.current_text()?;
        let hunks = diff_hunks(&before, new);
        if hunks.is_empty() {
            return None;
        }
        {
            let active = self.doc.as_mut()?;
            let mut txn = active.doc.transact_mut_with(LOCAL_ORIGIN);
            apply_hunks(&active.ytext, &mut txn, &before, &hunks);
        }
        // 单事务恒产出单个 update 事件；空 update（hunk 全被钳掉）不上行。
        // 多条即不变式被破坏（静默丢弃会丢内容），debug 构建当场暴露
        let msgs = {
            let mut sink = self.outbox.lock().expect("outbox 锁不可重入");
            sink.drain(..).collect::<Vec<_>>()
        };
        let mut msgs = msgs.into_iter().filter(|m| m.len() > 2);
        let first = msgs.next();
        debug_assert!(msgs.next().is_none(), "单事务应恒产出单个 update 消息");
        first
    }

    /// 文档静默对齐到指定文本：差分以 [`REMOTE_ORIGIN`] 事务合入，update 订阅按 origin
    /// 过滤不进 outbox（不广播）。保存合流回喂专用——合流结果含对端已落盘内容，若走
    /// `local_edit` 会把对端内容当本端新增广播，对端文档出现重复文本。
    pub fn align_silent(&mut self, text: &str) -> bool {
        let Some(before) = self.current_text() else { return false };
        let hunks = diff_hunks(&before, text);
        if hunks.is_empty() {
            return false;
        }
        let Some(active) = self.doc.as_mut() else { return false };
        let mut txn = active.doc.transact_mut_with(REMOTE_ORIGIN);
        apply_hunks(&active.ytext, &mut txn, &before, &hunks);
        true
    }

    /// 周期反熵/重连/收 Resync 后的握手：SyncStep1 索取对端状态（未打开 = None）。
    pub fn sync_step1(&mut self) -> Option<Vec<u8>> {
        let active = self.doc.as_ref()?;
        let mut msg = Vec::new();
        write_var_uint(&mut msg, SYNC_MSG_STEP1);
        write_var_bytes(&mut msg, &state_vector_v1(&active.doc));
        Some(encode_note_sync(&active.tag, &msg))
    }

    /// 入站 note-sync 通道帧（payload 已 base64 解码）。Sync/Baseline 帧无论最终是否应用，
    /// 其 tag.seq 都先记入房间序号（对齐提案序号取更高序的语义）。
    pub fn apply_remote(&mut self, peer_id: &str, file: &str, payload: &[u8]) -> RemoteOutcome {
        let Some(frame) = decode_note_payload(payload) else {
            return RemoteOutcome::default();
        };
        match frame {
            NoteFrame::Resync { .. } => {
                // 对端消费过慢被裁剪：重发本端握手（对端同样回发，双向补齐缺失增量）
                let mut out = RemoteOutcome::default();
                if self.matches_active(file) {
                    if let Some(step1) = self.sync_step1() {
                        out.reply.push(step1);
                    }
                }
                out
            }
            NoteFrame::Relocate { old_path, new_path } => {
                self.relocate(&old_path, &new_path);
                RemoteOutcome {
                    relocated: Some((old_path, new_path)),
                    ..RemoteOutcome::default()
                }
            }
            NoteFrame::Sync { tag, msg } => {
                self.remember_room_seq(file, tag.seq);
                self.receive_sync_frame(peer_id, file, tag, msg)
            }
            NoteFrame::Baseline { tag, baseline_text, current_text } => {
                self.remember_room_seq(file, tag.seq);
                self.receive_baseline_frame(peer_id, file, tag, baseline_text, current_text)
            }
        }
    }

    /// 登记该正文已落盘：推进三方合并的共同祖先（不动 tag）。
    pub fn mark_disk_write(&mut self, file: &str, flushed_text: &str) {
        if let Some(active) = self.doc.as_mut() {
            if active.file == file {
                active.last_flushed = flushed_text.to_owned();
            }
        }
    }

    /// 本端 awareness 状态（`user_json` = `{"name":..,"color":..,"colorLight":..}` 片段）。
    /// 状态 JSON 未变化返回 None；返回值 = 壳上行的 awareness 增量编码。
    /// 距上次上行不足 [`AWARENESS_MIN_INTERVAL`] 的变化只记录不产帧（拖选连续变化），
    /// 最新一份由 [`flush_awareness`](Self::flush_awareness) 补发。
    pub fn set_local_state(
        &mut self,
        user_json: &str,
        selection: Option<(u32, u32)>,
    ) -> Option<Vec<u8>> {
        self.doc.as_ref()?;
        let state = local_state_json_of(user_json, selection);
        if self.local_state_json.as_deref() == Some(state.as_str()) {
            return None;
        }
        self.local_user_json = Some(user_json.to_owned());
        self.local_selection = selection;
        self.local_state_json = Some(state);
        let now = Instant::now();
        if self
            .awareness_sent_at
            .is_some_and(|at| now.duration_since(at) < AWARENESS_MIN_INTERVAL)
        {
            return None;
        }
        self.emit_local_state(now)
    }

    /// 补发被限频压下的本端状态（壳周期搭载）：无待发变化或未到间隔 = None。
    /// `now` 由壳传入便于测试。
    pub fn flush_awareness(&mut self, now: Instant) -> Option<Vec<u8>> {
        self.doc.as_ref()?;
        if self.local_state_json.is_none() || self.local_state_json == self.local_sent_json {
            return None;
        }
        if self
            .awareness_sent_at
            .is_some_and(|at| now.duration_since(at) < AWARENESS_MIN_INTERVAL)
        {
            return None;
        }
        self.emit_local_state(now)
    }

    /// 是否有被限频压下、尚未上行的本端状态（壳据此保证补发拍不缺席）。
    pub fn has_pending_awareness(&self) -> bool {
        self.doc.is_some()
            && self.local_state_json.is_some()
            && self.local_state_json != self.local_sent_json
    }

    /// 强制重发本端 awareness 身份帧（重连后对端旧条目将在 30s 过期，主动补发维持光标可见）。
    /// 不受限频压制：身份帧缺失会让本端在对端整段不可见。
    pub fn resend_awareness(&mut self) -> Option<Vec<u8>> {
        let json = self.local_user_json.clone()?;
        self.local_state_json = Some(local_state_json_of(&json, self.local_selection));
        self.emit_local_state(Instant::now())
    }

    /// 产出本端身份帧：clock 递增、记账上行时刻与已上行状态。
    fn emit_local_state(&mut self, now: Instant) -> Option<Vec<u8>> {
        let state = self.local_state_json.clone()?;
        let client_id = self.doc.as_ref()?.doc.client_id().get();
        self.local_clock += 1;
        self.awareness_sent_at = Some(now);
        self.local_sent_json = Some(state.clone());
        Some(encode_awareness_update(&[AwarenessEntry {
            client_id,
            clock: self.local_clock,
            state_json: state,
        }]))
    }

    /// 到期保活：重发本端身份帧（无文档 / 未发过身份 / 未到间隔 = None）。
    /// 空闲端不重发就被对端按 [`AWARENESS_TIMEOUT`] 剔除（互见消失）；`now` 由壳传入便于测试。
    pub fn keepalive_if_due(&mut self, now: Instant) -> Option<Vec<u8>> {
        self.doc.as_ref()?;
        if !self
            .awareness_sent_at
            .is_none_or(|at| now.duration_since(at) >= AWARENESS_KEEPALIVE)
        {
            return None;
        }
        self.resend_awareness()
    }

    /// 合入远端 awareness（只应用不回发，防回环）。clock 已记忆的更小值丢弃；**同 clock
    /// 视为保活重发照常应用**（y-protocols 心跳重发不递增 clock，严格过滤会把空闲对端的
    /// 身份帧全丢——对端表现为「不可见」）；`null` 移除该端（不受 clock 门槛）；本端
    /// clientID 跳过；解码失败整体丢弃。`peer_id` = 帧来源连接（名册剔除时据此移条目）。
    pub fn apply_awareness(&mut self, peer_id: &str, payload: &[u8], now: Instant) {
        let Some(entries) = decode_awareness_update(payload) else { return };
        let Some(local) = self
            .doc
            .as_ref()
            .map(|d| d.doc.client_id().get())
        else {
            return;
        };
        for entry in entries {
            if entry.client_id == local {
                continue;
            }
            if entry.state_json == "null" {
                self.remote_states.remove(&entry.client_id);
                continue;
            }
            let fresh = self
                .remote_states
                .get(&entry.client_id)
                .map(|s| entry.clock >= s.clock)
                .unwrap_or(true);
            if !fresh {
                continue;
            }
            self.remote_states.insert(
                entry.client_id,
                RemoteState {
                    clock: entry.clock,
                    state_json: entry.state_json,
                    seen: now,
                    peer_id: peer_id.to_owned(),
                },
            );
        }
    }

    /// 对端连接消失：移除其上报过的全部 awareness 条目（对端退出 / 断网即时摘除光标，
    /// 不必等 [`AWARENESS_TIMEOUT`]）。返回是否有条目被移除。
    pub fn drop_peer(&mut self, peer_id: &str) -> bool {
        let before = self.remote_states.len();
        self.remote_states.retain(|_, s| s.peer_id != peer_id);
        self.remote_states.len() != before
    }

    /// 远端光标（按 clientID 稳定排序）；超时未刷新的端先剔除。
    pub fn remote_states(&mut self, now: Instant) -> Vec<RemoteCursor> {
        self.remote_states
            .retain(|_, s| now.duration_since(s.seen) <= AWARENESS_TIMEOUT);
        let mut cursors: Vec<(u64, RemoteCursor)> = self
            .remote_states
            .iter()
            .map(|(id, s)| (*id, cursor_from_state(*id, &s.state_json)))
            .collect();
        cursors.sort_by_key(|(id, _)| *id);
        cursors.into_iter().map(|(_, c)| c).collect()
    }

    // ---------- 入站帧内部分支 ----------

    fn matches_active(&self, file: &str) -> bool {
        self.doc.as_ref().is_some_and(|d| d.file == file)
    }

    fn receive_sync_frame(
        &mut self,
        peer_id: &str,
        file: &str,
        tag: DocTag,
        msg: Vec<u8>,
    ) -> RemoteOutcome {
        let Some(local_tag) = self
            .doc
            .as_ref()
            .filter(|d| d.file == file)
            .map(|d| d.tag.clone())
        else {
            return RemoteOutcome::default();
        };
        if !local_tag.same_content(&tag) || self.incompatible.contains(&peer_key(peer_id, file)) {
            // 异正文基线共享 seed 时钟空间，合并会按本地状态向量截断对端 struct（尾部混入异文本/
            // 静默分歧）：只回通告、不做跨基线合并。
            let mut out = RemoteOutcome::default();
            if let Some(frame) = self.reply_baseline(file) {
                out.reply.push(frame);
            }
            return out;
        }
        let mut reader = Reader::new(&msg);
        let Some(kind) = read_var_uint(&mut reader) else {
            return RemoteOutcome::default();
        };
        match kind {
            SYNC_MSG_STEP1 => {
                let Some(sv_bytes) = read_var_bytes(&mut reader) else {
                    return RemoteOutcome::default();
                };
                let Ok(sv) = StateVector::decode_v1(sv_bytes) else {
                    return RemoteOutcome::default();
                };
                let doc = &self.doc.as_ref().expect("上方已确认活动文档").doc;
                let txn = doc.transact();
                let diff = txn.encode_state_as_update_v1(&sv);
                drop(txn);
                let mut reply = Vec::new();
                write_var_uint(&mut reply, SYNC_MSG_STEP2);
                write_var_bytes(&mut reply, &diff);
                let mut out = RemoteOutcome::default();
                out.reply.push(encode_note_sync(&local_tag, &reply));
                out
            }
            SYNC_MSG_STEP2 | SYNC_MSG_UPDATE => {
                let Some(update_bytes) = read_var_bytes(&mut reader) else {
                    return RemoteOutcome::default();
                };
                let Ok(update) = Update::decode_v1(update_bytes) else {
                    // 乱序/格式异常的消息：丢弃，下一轮握手兜底收敛
                    return RemoteOutcome::default();
                };
                let before = self.current_text().expect("上方已确认活动文档");
                {
                    let doc = &self.doc.as_ref().expect("上方已确认活动文档").doc;
                    let mut txn = doc.transact_mut_with(REMOTE_ORIGIN);
                    // 集成失败（如引用未知的 parent）时按旧语义整条丢弃，靠后续握手补齐
                    let _ = txn.apply_update(update);
                }
                let after = self.current_text().expect("上方已确认活动文档");
                let doc_changed = before != after;
                RemoteOutcome {
                    doc_changed,
                    text: doc_changed.then_some(after),
                    ..RemoteOutcome::default()
                }
            }
            _ => RemoteOutcome::default(),
        }
    }

    fn receive_baseline_frame(
        &mut self,
        peer_id: &str,
        file: &str,
        tag: DocTag,
        baseline_text: String,
        current_text: String,
    ) -> RemoteOutcome {
        // 基线通告自带正文：逐字符复核内容标识，防散列碰撞/伪造把异正文当同基线合并
        if baseline_id_of(&baseline_text) != tag.id {
            return RemoteOutcome::default();
        }
        let Some((local_tag, baseline_local)) = self
            .doc
            .as_ref()
            .filter(|d| d.file == file)
            .map(|d| (d.tag.clone(), d.baseline_text.clone()))
        else {
            return RemoteOutcome::default();
        };

        if local_tag.same_content(&tag) {
            if baseline_text == baseline_local {
                // 同基线正文：字节一致的 seed，普通增量合并即可（无需重建）；
                // 若此前因标识碰撞把它记成不兼容，回一次通告让对方也解除（否则单向永久拉黑）
                let mut out = RemoteOutcome::default();
                if self.incompatible.remove(&peer_key(peer_id, file)) {
                    if let Some(frame) = self.reply_baseline(file) {
                        out.reply.push(frame);
                    }
                }
                return out;
            }
            // 同内容标识但基线正文不同：标识碰撞或伪造。同步帧只带标识（不带正文），这里是唯一
            // 防线：拉黑后其带该标识的同步帧一律不应用。不回通告——双方互拉黑时回通告只会形成
            // 2s 互刷，改为基线重建后重新复核。
            self.incompatible.insert(peer_key(peer_id, file));
            return RemoteOutcome::default();
        }
        if tag.compare(&local_tag) <= std::cmp::Ordering::Equal {
            let mut out = RemoteOutcome::default();
            if let Some(frame) = self.reply_baseline(file) {
                out.reply.push(frame);
            }
            return out;
        }
        self.adopt_baseline(&tag, &current_text)
    }

    /// 采纳更高序基线：以本端最近落盘正文为共同祖先三方合并后整体重建（非重叠两侧改动都保留，
    /// 同区间冲突取本端——与保存合流的裁决方向一致：用户在途输入优先，服务端另有对端内容不丢）。
    /// 祖先取 `last_flushed` 而非通告基线：两端基线 id 不同（id 即基线正文
    /// 散列）意味着没有真正的共同祖先，而通告方正文 = 服务端内容（⊇ 本端已落盘）+ 对端编辑，
    /// 以落盘正文为 base 时差异侧只含对端编辑，本端未落盘输入不冲突即保留。`last_flushed`
    /// 不随重建推进：合并结果未必在磁盘上。
    fn adopt_baseline(&mut self, tag: &DocTag, current_text: &str) -> RemoteOutcome {
        let file = self.doc.as_ref().expect("采纳前已确认活动文档").file.clone();
        let current = self.current_text().expect("上方已确认活动文档");
        let last_flushed = self
            .doc
            .as_ref()
            .map(|d| d.last_flushed.clone())
            .unwrap_or_default();
        let merged = merge3(&last_flushed, &current, current_text, ConflictSide::Ours);
        let same_baseline = merged == current_text && baseline_id_of(current_text) == tag.id;
        let next_tag = if same_baseline {
            tag.clone()
        } else {
            DocTag {
                seq: self.next_seq(&file),
                author: self.author.clone(),
                id: baseline_id_of(&merged),
            }
        };
        self.rebuild(&file, &merged, next_tag.clone());
        // 基线已替换：此前基于旧基线的实证不兼容结论作废，由后续通告重新复核
        self.clear_incompatible(&file);
        let mut out = RemoteOutcome {
            doc_changed: true,
            text: Some(merged.clone()),
            ..RemoteOutcome::default()
        };
        if !same_baseline {
            // 以更高序发布合并文本（房间据此采纳同一 seed），避免两文档各带不同 seed 继续交换状态
            out.reply.push(encode_note_baseline(&next_tag, &merged, &merged));
        }
        if let Some(step1) = self.sync_step1() {
            out.reply.push(step1);
        }
        // 新 doc 新 clientID：本端身份需重发（选区指向旧文本，随重建作废）
        out.awareness = self.resend_local_state();
        out
    }

    /// 整体重建活动文档为「文本 text + 标签 tag」：旧实例丢弃、新实例以确定性 seed 建立基线。
    fn rebuild(&mut self, file: &str, text: &str, tag: DocTag) {
        let last_flushed = self
            .doc
            .as_ref()
            .map(|d| d.last_flushed.clone())
            .unwrap_or_default();
        let (doc, ytext) = spawn_doc(text, &self.outbox);
        self.doc = Some(ActiveDoc {
            file: file.to_owned(),
            doc,
            ytext,
            tag: tag.clone(),
            baseline_text: text.to_owned(),
            last_flushed,
        });
        self.remember_room_seq(file, tag.seq);
        // 远端光标随旧实例作废；本端 clock 归零，在新 clientID 下重发身份
        self.remote_states.clear();
        self.local_state_json = None;
        self.local_sent_json = None;
        self.local_selection = None;
        self.local_clock = 0;
    }

    /// 回通告本端基线（2s 限频）：让异基线对端采纳本端，而不是做跨基线合并。
    fn reply_baseline(&mut self, file: &str) -> Option<Vec<u8>> {
        let now = Instant::now();
        if let Some(last) = self.baseline_reply_at.get(file) {
            if now.duration_since(*last) < BASELINE_REPLY_INTERVAL {
                return None;
            }
        }
        self.baseline_reply_at.insert(file.to_owned(), now);
        let active = self.doc.as_ref()?;
        let current = {
            let txn = active.doc.transact();
            active.ytext.get_string(&txn)
        };
        Some(encode_note_baseline(&active.tag, &active.baseline_text, &current))
    }

    /// 重建后的本端身份重发：保留 user（从未设置过 = None），选区随旧文本作废，
    /// clock 从 1 起（`rebuild` 已归零）。不受限频压制（新 clientID 的首帧必须立刻上行）。
    fn resend_local_state(&mut self) -> Option<Vec<u8>> {
        let user_json = self.local_user_json.clone()?;
        self.local_selection = None;
        self.local_state_json = Some(local_state_json_of(&user_json, None));
        self.emit_local_state(Instant::now())
    }

    // ---------- 序号与黑名单 ----------

    fn remember_room_seq(&mut self, file: &str, seq: u64) {
        let slot = self.room_seqs.entry(file.to_owned()).or_insert(0);
        if seq > *slot {
            *slot = seq;
        }
    }

    /// 提案序号 = 房间已知最大序号（含本端已发）+ 1，提案即记入。
    fn next_seq(&mut self, file: &str) -> u64 {
        let next = self.room_seqs.get(file).copied().unwrap_or(0).saturating_add(1);
        self.remember_room_seq(file, next);
        next
    }

    fn clear_incompatible(&mut self, file: &str) {
        let suffix = format!("\0{file}");
        self.incompatible.retain(|key| !key.ends_with(&suffix));
    }

    /// 文件换路跟随（远端 Relocate 帧 / 改名广播）：活动文档挂靠路径与该路径的
    /// 序号、限频记忆一并改写（文档内容不动；含目录换路的前缀跟随，路径不符仅迁移记忆）。
    pub fn relocate(&mut self, old: &str, new: &str) {
        let map = |p: &str| -> Option<String> {
            if p == old {
                Some(new.to_owned())
            } else {
                p.strip_prefix(old)
                    .and_then(|r| r.strip_prefix('/'))
                    .map(|rest| format!("{new}/{rest}"))
            }
        };
        if let Some(active) = self.doc.as_mut() {
            if let Some(next) = map(&active.file) {
                active.file = next;
            }
        }
        // 序号记忆取两侧更大值（序号回退会引发额外一轮基线协商）；限频记点保留已有
        let moved_seqs: Vec<(String, String, u64)> = self
            .room_seqs
            .iter()
            .filter_map(|(k, v)| map(k).map(|n| (k.clone(), n, *v)))
            .collect();
        for (old_key, new_key, value) in moved_seqs {
            self.room_seqs.remove(&old_key);
            let slot = self.room_seqs.entry(new_key).or_insert(0);
            if value > *slot {
                *slot = value;
            }
        }
        let moved_replies: Vec<(String, String)> = self
            .baseline_reply_at
            .keys()
            .filter_map(|k| map(k).map(|n| (k.clone(), n)))
            .collect();
        for (old_key, new_key) in moved_replies {
            let at = self.baseline_reply_at.remove(&old_key).expect("键刚查到");
            self.baseline_reply_at.entry(new_key).or_insert(at);
        }
        // 实证黑名单随路径身份迁移：旧路径键滞留会让 clear_incompatible(新路径) 清不掉，
        // 且改路后对旧键的 sync 屏蔽与文档实际路径脱节
        let moved_blacklist: Vec<(String, String)> = self
            .incompatible
            .iter()
            .filter_map(|k| {
                let (peer, file) = k.split_once('\0')?;
                map(file).map(|n| (k.clone(), peer_key(peer, &n)))
            })
            .collect();
        for (old_key, new_key) in moved_blacklist {
            self.incompatible.remove(&old_key);
            self.incompatible.insert(new_key);
        }
    }
}

fn peer_key(peer_id: &str, file: &str) -> String {
    format!("{peer_id}\0{file}")
}

/// 把差分 hunks 依次合入 ytext。hunk 索引全基于差分前文本：多 hunk 依次应用时按前序
/// 净变化（UTF-16 单位）平移，否则后续 hunk 落在错位区间（差分 hunk 升序不重叠，前缀
/// 平移量恒非负）。
fn apply_hunks(
    ytext: &yrs::TextRef,
    txn: &mut yrs::TransactionMut,
    before: &str,
    hunks: &[crate::merge3::TextHunk],
) {
    let mut shift: i64 = 0;
    for hunk in hunks {
        let at = yrs_index_for_byte(before, hunk.at.min(before.len())) as i64 + shift;
        let removed = if hunk.remove > 0 {
            let end = yrs_index_for_byte(before, (hunk.at + hunk.remove).min(before.len())) as i64 + shift;
            let span = (end - at).max(0);
            if span > 0 {
                ytext.remove_range(txn, at as u32, span as u32);
            }
            span
        } else {
            0
        };
        if !hunk.insert.is_empty() {
            ytext.insert(txn, at as u32, &hunk.insert);
        }
        shift += hunk.insert.encode_utf16().count() as i64 - removed;
    }
}

/// 创建活动文档：随机 clientID + UTF-16 索引单位；先注入确定性 seed 再挂更新订阅（seed 不广播）。
fn spawn_doc(text: &str, outbox: &Arc<Mutex<Vec<Vec<u8>>>>) -> (Doc, yrs::TextRef) {
    let doc = Doc::with_options(Options {
        offset_kind: OffsetKind::Utf16,
        ..Options::default()
    });
    let ytext = doc.get_or_insert_text("text");
    let seed = baseline_seed_update(text);
    let mut txn = doc.transact_mut();
    let update = Update::decode_v1(&seed).expect("自有 seed 编码恒可解码");
    txn.apply_update(update).expect("自有 seed 恒可集成");
    drop(txn);
    let sink = outbox.clone();
    doc.observe_update_v1("note-outbox", move |txn, event| {
        if txn
            .origin()
            .is_some_and(|o| o.as_ref() == REMOTE_ORIGIN.as_bytes())
        {
            return;
        }
        let mut msg = Vec::new();
        write_var_uint(&mut msg, SYNC_MSG_UPDATE);
        write_var_bytes(&mut msg, &event.update);
        sink.lock().expect("outbox 锁不可重入").push(msg);
    })
    .expect("单线程内挂订阅不会与并发事务冲突");
    (doc, ytext)
}

/// 以固定 seed clientID 把正文编码成确定的 Yjs update：同正文各端字节一致，基线合并幂等。
fn baseline_seed_update(text: &str) -> Vec<u8> {
    let doc = Doc::with_options(Options {
        client_id: ClientID::new(SEED_CLIENT_ID),
        offset_kind: OffsetKind::Utf16,
        ..Options::default()
    });
    let ytext = doc.get_or_insert_text("text");
    {
        let mut txn = doc.transact_mut();
        ytext.insert(&mut txn, 0, text);
    }
    let txn = doc.transact();
    txn.encode_state_as_update_v1(&StateVector::new(Default::default()))
}

fn state_vector_v1(doc: &Doc) -> Vec<u8> {
    let txn = doc.transact();
    let sv = txn.state_vector();
    let mut encoder = EncoderV1::new();
    sv.encode(&mut encoder);
    encoder.to_vec()
}

/// Rust 字节偏移 → yrs TextRef 索引（文档为 UTF-16 索引单位；落在多字节字符内部取该字符起点）。
fn yrs_index_for_byte(text: &str, byte: usize) -> u32 {
    byte_to_utf16_offset(text, byte) as u32
}

/// 本端 awareness state JSON（`user_json` 已是裸对象片段；无选区时不带 selection 键）。
fn local_state_json_of(user_json: &str, selection: Option<(u32, u32)>) -> String {
    match selection {
        Some((anchor, head)) => {
            format!(r##"{{"user":{user_json},"selection":{{"anchor":{anchor},"head":{head}}}}}"##)
        }
        None => format!(r##"{{"user":{user_json}}}"##),
    }
}

fn cursor_from_state(client_id: u64, state_json: &str) -> RemoteCursor {
    let value: Value = serde_json::from_str(state_json).unwrap_or(Value::Null);
    let user = value.get("user");
    let selection = value.get("selection");
    RemoteCursor {
        client_id,
        name: user
            .and_then(|u| u.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        color: user
            .and_then(|u| u.get("color"))
            .and_then(Value::as_str)
            .unwrap_or(FALLBACK_CURSOR_COLOR)
            .to_owned(),
        anchor: selection
            .and_then(|s| s.get("anchor"))
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok()),
        head: selection
            .and_then(|s| s.get("head"))
            .and_then(Value::as_u64)
            .and_then(|v| u32::try_from(v).ok()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space_frame::{encode_note_relocate, encode_note_resync};

    const GOLDEN_FULL_UPDATE: &str =
        "0101b2f21900040104746578741de4b880e6aeb5e59fbae7babfe6ada3e696870ae7acace4ba8ce8a18c0a00";
    const GOLDEN_INCREMENTAL_UPDATE: &str =
        "0101b2f2190b84b2f2190a100ae696b0e5a29ee7acace4b889e8a18c00";
    const GOLDEN_AWARENESS: &str =
        "01eac1a3940f01677b2275736572223a7b226e616d65223a22e5b08fe6988e222c22636f6c6f72223a2223653036633735222c22636f6c6f724c69676874223a22236530366337353333227d2c2273656c656374696f6e223a7b22616e63686f72223a332c2268656164223a377d7d";
    const BASELINE_TEXT: &str = "一段基线正文\n第二行\n";

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("测试用例恒为合法 hex"))
            .collect()
    }

    fn hex(bytes: impl AsRef<[u8]>) -> String {
        bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
    }

    fn sync_frame(tag: &DocTag, msg: &[u8]) -> Vec<u8> {
        encode_note_sync(tag, msg)
    }

    #[test]
    fn seed_update与yjs侧字节一致() {
        // golden = yjs baselineSeedUpdate（clientID=424242）实测，仅 clientID varUint 不同
        let update = baseline_seed_update(BASELINE_TEXT);
        assert_eq!(
            hex(&update),
            "01010100040104746578741de4b880e6aeb5e59fbae7babfe6ada3e696870ae7acace4ba8ce8a18c0a00"
        );
    }

    #[test]
    fn yjs全量与增量update解码互通() {
        // golden update 由 yjs（clientID=424242）实测产出。协议上异 id 的 sync 帧会被拒收
        // （异正文基线不可跨基线合并），故字节互通在文档绑定层验证：应用后文本即正文。
        let doc = Doc::with_options(Options {
            offset_kind: OffsetKind::Utf16,
            ..Options::default()
        });
        let ytext = doc.get_or_insert_text("text");
        {
            let mut txn = doc.transact_mut();
            txn.apply_update(Update::decode_v1(&unhex(GOLDEN_FULL_UPDATE)).unwrap())
                .unwrap();
        }
        let txn = doc.transact();
        assert_eq!(ytext.get_string(&txn), BASELINE_TEXT);
        drop(txn);
        {
            let mut txn = doc.transact_mut();
            txn.apply_update(Update::decode_v1(&unhex(GOLDEN_INCREMENTAL_UPDATE)).unwrap())
                .unwrap();
        }
        let txn = doc.transact();
        assert_eq!(ytext.get_string(&txn), "一段基线正文\n第二行\n\n新增第三行");
    }

    #[test]
    fn yjs_awareness_update解析与门槛建制() {
        let mut nc = NoteCollab::new("a".to_owned());
        nc.open("a.md", "");
        let now = Instant::now();
        nc.apply_awareness("p1", &unhex(GOLDEN_AWARENESS), now);
        let states = nc.remote_states(now);
        assert_eq!(states.len(), 1);
        assert_eq!(states[0].name, "小明");
        assert_eq!(states[0].color, "#e06c75");
        assert_eq!(states[0].anchor, Some(3));
        assert_eq!(states[0].head, Some(7));

        let golden_id = decode_awareness_update(&unhex(GOLDEN_AWARENESS)).unwrap()[0].client_id;
        // 同 clock 重发照常应用（y-protocols 心跳不递增 clock）：seen 刷新为重发时刻
        nc.apply_awareness("p1", &unhex(GOLDEN_AWARENESS), now + Duration::from_secs(10));
        assert_eq!(nc.remote_states(now + Duration::from_secs(10))[0].name, "小明");
        assert_eq!(nc.remote_states(now + Duration::from_secs(31)).len(), 1,
            "首次应用 31s 后本应超时，保活重发把存活窗口顶到重发时刻");
        // 更小 clock 丢弃：状态不翻转为「旧」
        let stale = encode_awareness_update(&[AwarenessEntry {
            client_id: golden_id,
            clock: 0,
            state_json: r##"{"user":{"name":"旧","color":"#000000"}}"##.to_owned(),
        }]);
        nc.apply_awareness("p1", &stale, now + Duration::from_secs(10));
        assert_eq!(nc.remote_states(now + Duration::from_secs(10))[0].name, "小明");
        // 更高 clock 被采纳
        let newer = encode_awareness_update(&[AwarenessEntry {
            client_id: golden_id,
            clock: 2,
            state_json: r##"{"user":{"name":"新","color":"#111111"}}"##.to_owned(),
        }]);
        nc.apply_awareness("p1", &newer, now);
        assert_eq!(nc.remote_states(now)[0].name, "新");
        // null 移除该端
        let removal = encode_awareness_update(&[AwarenessEntry {
            client_id: golden_id,
            clock: 3,
            state_json: "null".to_owned(),
        }]);
        nc.apply_awareness("p1", &removal, now);
        assert!(nc.remote_states(now).is_empty());
        // 超 30s 剔除
        nc.apply_awareness("p1", &newer, now);
        assert!(nc.remote_states(now + Duration::from_secs(31)).is_empty());

        // 本端状态跳过：set_local_state 得到的 clientID 不进远端表
        let local_payload = nc
            .set_local_state(r##"{"name":"本端","color":"#123456"}"##, Some((1, 2)))
            .unwrap();
        let local_id = decode_awareness_update(&local_payload).unwrap()[0].client_id;
        let echo = encode_awareness_update(&[AwarenessEntry {
            client_id: local_id,
            clock: 9,
            state_json: r##"{"user":{"name":"回声","color":"#000000"}}"##.to_owned(),
        }]);
        nc.apply_awareness("p1", &echo, now);
        assert!(nc.remote_states(now).is_empty());
    }

    #[test]
    fn 本端sync_step1帧结构() {
        let mut nc = NoteCollab::new("a".to_owned());
        let opened = nc.open("a.md", "");
        let Some(NoteFrame::Sync { tag, msg }) = decode_note_payload(&opened.sync_step1) else {
            panic!("sync_step1 应为 SYNC 帧");
        };
        assert_eq!(tag.author, "a");
        assert_eq!(tag.id, baseline_id_of(""));
        assert_eq!(msg[0], SYNC_MSG_STEP1 as u8);
        let mut reader = Reader::new(&msg);
        assert_eq!(read_var_uint(&mut reader), Some(SYNC_MSG_STEP1));
        let sv = read_var_bytes(&mut reader).expect("状态向量包裹完整");
        assert!(!sv.is_empty());
        assert!(StateVector::decode_v1(sv).is_ok());
        // open 的 seed 在挂订阅前注入，outbox 无残留
        assert!(nc.outbox.lock().unwrap().is_empty());
    }

    #[test]
    fn yrs索引为utf16单位且hunk换算正确() {
        let mut nc = NoteCollab::new("a".to_owned());
        let text = "中文\na😀尾\n"; // 8 个 UTF-16 单位、16 字节
        nc.open("f.md", text);
        {
            let active = nc.doc.as_ref().unwrap();
            let txn = active.doc.transact();
            assert_eq!(active.ytext.len(&txn), 8);
        }
        // 在 😀 之后插入（hunk 锚点字节跨过多字节字符）
        let edited = "中文\na😀插入尾\n";
        assert!(nc.local_edit(edited).is_some());
        assert_eq!(nc.current_text().as_deref(), Some(edited));
        // 删除 😀 与插入段（remove 区间含 2 单位的代理对）
        let removed = "中文\na尾\n";
        assert!(nc.local_edit(removed).is_some());
        assert_eq!(nc.current_text().as_deref(), Some(removed));
        // 字节落在 😀 内部时向下取整到其 UTF-16 起点
        assert_eq!(yrs_index_for_byte("a😀尾", 3), 1);
        assert_eq!(yrs_index_for_byte("a😀尾", 5), 3);
    }

    #[test]
    fn 多hunk编辑按序补偿坐标() {
        // 两个 hunk 落在不同位置：后一 hunk 的 yrs 索引须补偿前一 hunk 的净长度变化，
        // 否则插入/删除落在错位区间（正文错乱）
        let mut nc = NoteCollab::new("a".to_owned());
        let before = "aa\nP\nbb\nQ\ncc\n";
        let after = "aa\nQ\nbb\nP\ncc\n";
        nc.open("f.md", before);
        assert!(nc.local_edit(after).is_some());
        assert_eq!(nc.current_text().as_deref(), Some(after));
    }

    #[test]
    fn 基线通告幂等与采纳() {
        let text = "v1\n";
        let mut a = NoteCollab::new("a".to_owned());
        a.open("f.md", text);
        // 同 id 同正文：幂等确认，无回复
        let peer_tag = DocTag { seq: 1, author: "p".to_owned(), id: baseline_id_of(text) };
        let out = a.apply_remote("p", "f.md", &encode_note_baseline(&peer_tag, text, text));
        assert!(out.reply.is_empty() && !out.doc_changed);
        // 更高 seq 异 id：本端无未落盘差异 → 直接采纳对方 tag（reply 只有重建后的握手）
        let theirs = "v2\n";
        let tag2 = DocTag { seq: 5, author: "p".to_owned(), id: baseline_id_of(theirs) };
        let out = a.apply_remote("p", "f.md", &encode_note_baseline(&tag2, theirs, theirs));
        assert!(out.doc_changed);
        assert_eq!(a.tag(), Some(&tag2));
        assert_eq!(a.current_text().as_deref(), Some(theirs));
        assert_eq!(out.reply.len(), 1);
        let Some(NoteFrame::Sync { tag: reply_tag, msg }) = decode_note_payload(&out.reply[0])
        else {
            panic!("重建后应回握手帧");
        };
        assert_eq!(reply_tag, tag2);
        assert_eq!(msg[0], SYNC_MSG_STEP1 as u8);
    }

    #[test]
    fn 异id同步帧拒收回通告且限频() {
        let mut a = NoteCollab::new("a".to_owned());
        a.open("f.md", "v1\n");
        let other = DocTag { seq: 1, author: "p".to_owned(), id: baseline_id_of("v2\n") };
        let sync = sync_frame(&other, &[0, 1, 2]);
        let out = a.apply_remote("p", "f.md", &sync);
        assert!(!out.doc_changed);
        assert_eq!(out.reply.len(), 1);
        let Some(NoteFrame::Baseline { tag, baseline_text, .. }) =
            decode_note_payload(&out.reply[0])
        else {
            panic!("拒收应回 baseline 通告帧");
        };
        assert_eq!(tag.author, "a");
        assert_eq!(baseline_text, "v1\n");
        // 2s 内第二次拒收：限频不回
        assert!(a.apply_remote("p", "f.md", &sync).reply.is_empty());
        // 另一实例上 compareTag 更低的异 id baseline 也走回通告
        let mut b = NoteCollab::new("a".to_owned());
        b.open("f.md", "v1\n");
        let low = DocTag { seq: 0, author: "z".to_owned(), id: baseline_id_of("v3\n") };
        let out = b.apply_remote("p", "f.md", &encode_note_baseline(&low, "v3\n", "v3\n"));
        assert_eq!(out.reply.len(), 1);
    }

    #[test]
    fn 同id异正文通告拉黑对端且回通告解除() {
        // 两端正文等长（同 UTF-16 units），对端增量的 origin 引用才可落在同 clock 空间上
        let text = "v1\n";
        let mut a = NoteCollab::new("a".to_owned());
        a.open("f.md", text);
        // 不自洽的通告（id 与其基线正文不匹配）：复核前即静默丢弃，不拉黑不回通告
        let forged = DocTag { seq: 9, author: "p".to_owned(), id: baseline_id_of(text) };
        let out = a.apply_remote(
            "p",
            "f.md",
            &encode_note_baseline(&forged, "异正文\n", "异正文\n"),
        );
        assert!(out.reply.is_empty() && !out.doc_changed);
        // 同 id 异正文的通告只在散列碰撞/自洽伪造时发生（此处模拟复核通过后的实证结论）：
        // 拉黑后其 sync 帧内容不被应用
        a.incompatible.insert(peer_key("p", "f.md"));
        let mut p = NoteCollab::new("p".to_owned());
        p.open("g.md", "x1\n");
        let p_msg = p.local_edit("x1改\n").unwrap();
        let out = a.apply_remote("p", "f.md", &sync_frame(&forged, &p_msg));
        assert!(!out.doc_changed);
        assert_eq!(a.current_text().as_deref(), Some(text));
        // 拉黑拒收同样回通告（限频记点），让坏基线对端有机会纠正
        assert_eq!(out.reply.len(), 1);
        // 合法对端的同 id 同正文通告解除拉黑；回通告被上面的限频拦下，仅解除实证结论
        let out = a.apply_remote("p", "f.md", &encode_note_baseline(&forged, text, text));
        assert!(out.reply.is_empty() && !out.doc_changed);
        // 解除后其 sync 帧恢复应用：delete set 按 ID 落在本端同区间 struct 上，正文被对端当前文本替换
        let out = a.apply_remote("p", "f.md", &sync_frame(&forged, &p_msg));
        assert!(out.doc_changed);
        assert_eq!(a.current_text().as_deref(), Some("x1改\n"));
    }

    #[test]
    fn 基线重建清除拉黑并重发身份() {
        let mut a = NoteCollab::new("a".to_owned());
        a.open("f.md", "v1\n");
        a.set_local_state(r##"{"name":"我","color":"#111111"}"##, Some((1, 2)))
            .unwrap();
        let forged = DocTag { seq: 9, author: "p".to_owned(), id: baseline_id_of("v1\n") };
        a.apply_remote(
            "p",
            "f.md",
            &encode_note_baseline(&forged, "异正文\n", "异正文\n"),
        );
        // 更高 seq 的合法通告 → 采纳重建 → 拉黑解除、awareness 以新 clientID 重发（选区随旧文本作废）
        let good = DocTag { seq: 20, author: "ok".to_owned(), id: baseline_id_of("新基线\n") };
        let out = a.apply_remote(
            "p",
            "f.md",
            &encode_note_baseline(&good, "新基线\n", "新基线\n"),
        );
        assert!(out.doc_changed);
        let awareness =
            decode_awareness_update(&out.awareness.expect("重建后应重发身份")).unwrap();
        assert_eq!(awareness[0].clock, 1);
        assert_eq!(
            awareness[0].state_json,
            r##"{"user":{"name":"我","color":"#111111"}}"##
        );
        // 拉黑已解除：同 id 的 sync 帧恢复应用
        let mut q = NoteCollab::new("q".to_owned());
        q.open("h.md", "新基线\n");
        let q_msg = q.local_edit("新基线改\n").unwrap();
        let out = a.apply_remote("q", "f.md", &sync_frame(&good, &q_msg));
        assert!(out.doc_changed);
        assert_eq!(a.current_text().as_deref(), Some("新基线改\n"));
    }

    #[test]
    fn 双实例互发收敛一致() {
        let mut a = NoteCollab::new("a".to_owned());
        let mut b = NoteCollab::new("b".to_owned());
        let base = "第一行\n第二行\n第三行\n";
        a.open("f.md", base);
        b.open("f.md", base);
        // 同正文 → 同基线 id → sync 帧互认
        let a_text = "第一行A\n第二行\n第三行\n";
        let a_msg = a.local_edit(a_text).unwrap();
        let b_text = "第一行\n第二行\n第三行B\n";
        let b_msg = b.local_edit(b_text).unwrap();
        b.apply_remote("pa", "f.md", &sync_frame(a.tag().unwrap(), &a_msg));
        a.apply_remote("pb", "f.md", &sync_frame(b.tag().unwrap(), &b_msg));
        assert_eq!(a.current_text(), b.current_text());
        assert_eq!(a.current_text().as_deref(), Some("第一行A\n第二行\n第三行B\n"));
        // 第二轮基于收敛后文本继续编辑
        let cur = a.current_text().unwrap();
        let next = cur.replace("第二行", "第二行A2");
        let msg = a.local_edit(&next).unwrap();
        let out = b.apply_remote("pa", "f.md", &sync_frame(a.tag().unwrap(), &msg));
        assert!(out.doc_changed);
        assert_eq!(out.text.as_deref(), Some(next.as_str()));
        assert_eq!(a.current_text(), b.current_text());
    }

    #[test]
    fn 落盘推进合并祖先() {
        let base = "a\nb\n";
        let local = "a\nb-local\n";
        let theirs = "a-theirs\nb\n";
        let frame_tag = DocTag { seq: 5, author: "p".to_owned(), id: baseline_id_of(theirs) };
        let frame = encode_note_baseline(&frame_tag, theirs, theirs);
        // 未落盘：以 v1 为共同祖先 → 两侧改动都保留、发布本端提案
        let mut a = NoteCollab::new("a".to_owned());
        a.open("f.md", base);
        a.local_edit(local);
        let out = a.apply_remote("p", "f.md", &frame);
        assert!(out.doc_changed);
        assert_eq!(a.current_text().as_deref(), Some("a-theirs\nb-local\n"));
        assert_eq!(a.tag().map(|t| (t.seq, t.author.as_str())), Some((6, "a")));
        assert_eq!(out.reply.len(), 2);
        // 落盘后：本地差异成为共同祖先 → 对端正文直接采纳
        let mut b = NoteCollab::new("a".to_owned());
        b.open("f.md", base);
        b.local_edit(local);
        b.mark_disk_write("f.md", local);
        let out = b.apply_remote("p", "f.md", &frame);
        assert!(out.doc_changed);
        assert_eq!(b.current_text().as_deref(), Some(theirs));
        assert_eq!(b.tag(), Some(&frame_tag));
        assert_eq!(out.reply.len(), 1);
        // 不相关文件的落盘不动活动文档祖先
        b.mark_disk_write("other.md", "无关\n");
        assert_eq!(b.tag(), Some(&frame_tag));
    }

    #[test]
    fn resync与relocate与序号记忆() {
        let mut a = NoteCollab::new("a".to_owned());
        a.open("f.md", "v1\n");
        // Resync → 重发握手
        let out = a.apply_remote("p", "f.md", &encode_note_resync(1));
        assert_eq!(out.reply.len(), 1);
        assert!(matches!(
            decode_note_payload(&out.reply[0]),
            Some(NoteFrame::Sync { msg, .. }) if msg[0] == SYNC_MSG_STEP1 as u8
        ));
        // Relocate → 交壳改挂路径，文档不动
        let out = a.apply_remote("p", "f.md", &encode_note_relocate("f.md", "g.md"));
        assert_eq!(out.relocated, Some(("f.md".to_owned(), "g.md".to_owned())));
        assert_eq!(a.current_text().as_deref(), Some("v1\n"));
        // 帧序号跨 close/open 记忆：对端宣称更高 seq 后，本端重新提案取更高序
        let higher = DocTag { seq: 40, author: "p".to_owned(), id: baseline_id_of("v1\n") };
        a.apply_remote("p", "f.md", &sync_frame(&higher, &[]));
        a.open("f.md", "v1\n");
        assert_eq!(a.tag().map(|t| t.seq), Some(41));
        // 其他文件的序号独立记忆
        let mut a2 = NoteCollab::new("a".to_owned());
        let other = DocTag { seq: 7, author: "p".to_owned(), id: baseline_id_of("x\n") };
        a2.apply_remote("p", "other.md", &sync_frame(&other, &[]));
        a2.open("other.md", "x\n");
        assert_eq!(a2.tag().map(|t| t.seq), Some(8));
    }

    #[test]
    fn 本端状态变化才发增量() {
        let mut nc = NoteCollab::new("a".to_owned());
        nc.open("f.md", "v1\n");
        let first = nc
            .set_local_state(r##"{"name":"我","color":"#111111"}"##, None)
            .unwrap();
        assert_eq!(decode_awareness_update(&first).unwrap()[0].clock, 1);
        assert!(nc
            .set_local_state(r##"{"name":"我","color":"#111111"}"##, None)
            .is_none());
        let with_sel = nc
            .set_local_state(r##"{"name":"我","color":"#111111"}"##, Some((3, 7)));
        // 紧接上一帧的选区变化被限频压下（状态已记录），由补发产出
        assert!(with_sel.is_none());
        let flushed = nc
            .flush_awareness(Instant::now() + AWARENESS_MIN_INTERVAL)
            .expect("到间隔应补发");
        let entry = &decode_awareness_update(&flushed).unwrap()[0];
        assert_eq!(entry.clock, 2);
        assert!(entry.state_json.ends_with(r##","selection":{"anchor":3,"head":7}}"##));
        assert!(
            nc.flush_awareness(Instant::now() + Duration::from_secs(1))
                .is_none(),
            "无待发变化不产帧"
        );
        // 未打开文档时不产出增量
        nc.close();
        assert!(nc
            .set_local_state(r##"{"name":"我","color":"#111111"}"##, None)
            .is_none());
    }

    /// 连续选区变化只上最新一份：限频期内仅记录，到间隔后补发一帧。
    #[test]
    fn 选区连续变化合并为最新一帧() {
        let mut nc = NoteCollab::new("a".to_owned());
        nc.open("f.md", "v1\n");
        let user = r##"{"name":"我","color":"#111111"}"##;
        assert!(nc.set_local_state(user, Some((1, 1))).is_some());
        assert!(nc.set_local_state(user, Some((2, 2))).is_none());
        assert!(nc.set_local_state(user, Some((3, 3))).is_none());
        let at = Instant::now() + AWARENESS_MIN_INTERVAL;
        let frame = nc.flush_awareness(at).expect("到间隔补发最新一份");
        let entry = &decode_awareness_update(&frame).unwrap()[0];
        assert_eq!(entry.clock, 2);
        assert!(entry.state_json.contains(r##""anchor":3,"head":3"##));
        assert!(nc.flush_awareness(at + Duration::from_secs(1)).is_none());
    }

    /// 对端连接消失即摘除其光标（名册驱动），其它对端条目与超时剔除互不影响。
    #[test]
    fn 对端断链即时摘除其光标() {
        let mut nc = NoteCollab::new("a".to_owned());
        nc.open("f.md", "v1\n");
        let now = Instant::now();
        let first = encode_awareness_update(&[AwarenessEntry {
            client_id: 11,
            clock: 1,
            state_json: r##"{"user":{"name":"甲","color":"#111111"}}"##.to_owned(),
        }]);
        let second = encode_awareness_update(&[AwarenessEntry {
            client_id: 22,
            clock: 1,
            state_json: r##"{"user":{"name":"乙","color":"#222222"}}"##.to_owned(),
        }]);
        nc.apply_awareness("peer-1", &first, now);
        nc.apply_awareness("peer-2", &second, now);
        assert_eq!(nc.remote_states(now).len(), 2);
        assert!(nc.drop_peer("peer-1"));
        let left = nc.remote_states(now);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].name, "乙");
        // 已摘除的连接与从未上报过的连接：无变化
        assert!(!nc.drop_peer("peer-1"));
        assert!(!nc.drop_peer("peer-9"));
    }

    /// 保活限频：身份已发未到期不重发；到期重发且 clock 递增；无文档或未发过身份恒不产帧。
    /// 时间戳记账为真实时钟（`set_local_state` 内盖戳），判定时刻以真实时钟加偏移构造。
    #[test]
    fn 保活到期才重发身份帧() {
        let mut nc = NoteCollab::new("a".to_owned());
        let t0 = Instant::now();
        assert!(nc.keepalive_if_due(t0).is_none(), "未打开文档不产帧");
        nc.open("f.md", "v1\n");
        assert!(nc.keepalive_if_due(t0).is_none(), "未发过身份不产帧");
        nc.set_local_state(r##"{"name":"我","color":"#111111"}"##, None)
            .unwrap();
        assert!(
            nc.keepalive_if_due(Instant::now() + Duration::from_secs(14))
                .is_none(),
            "未到期不重发"
        );
        let again = nc
            .keepalive_if_due(Instant::now() + Duration::from_secs(15))
            .expect("到期应重发");
        assert_eq!(decode_awareness_update(&again).unwrap()[0].clock, 2);
    }

    /// 选区偏移 u32 往返：超 u16 的长文选区编解码不丢失。
    #[test]
    fn 选区偏移u32往返() {
        let mut a = NoteCollab::new("a".to_owned());
        a.open("f.md", "v1\n");
        let frame = a
            .set_local_state(r##"{"name":"我","color":"#111111"}"##, Some((70_000, 70_010)))
            .unwrap();
        let mut b = NoteCollab::new("b".to_owned());
        b.open("f.md", "v1\n");
        let now = Instant::now();
        b.apply_awareness("pa", &frame, now);
        let cursors = b.remote_states(now);
        assert_eq!(cursors.len(), 1);
        assert_eq!(cursors[0].anchor, Some(70_000));
        assert_eq!(cursors[0].head, Some(70_010));
    }
}
