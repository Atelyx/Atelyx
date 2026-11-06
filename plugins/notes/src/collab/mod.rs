//! 笔记协作：可靠通道收发 yrs 正文帧，瞬时通道收发在场光标；通道由宿主按房间投递。

pub mod ephemeral;
pub mod frame;
pub mod presence;
pub mod room;

use std::time::Instant;

use base64::Engine as _;
use serde_json::{json, Value};

use atelyx_plugin_protocol::method::{SpaceContainerKind, SpaceSelfIdentity};

use crate::collab::frame::{
    byte_to_utf16_offset, encode_note_sync, utf16_to_byte_offset,
};
use crate::collab::presence::{DocPresenceChannel, Identity};
use crate::collab::room::{NoteCollab, RemoteOutcome};
use crate::model::RemoteCursor;

/// 正文帧通道名（可靠档：宿主在断线期缓存，重连后按序补投）。
pub const SYNC_CHANNEL: &str = "doc-sync";

/// 名册未给出身份时的回落名。
const FALLBACK_NAME: &str = "用户";
/// 名册未给出成员色时的回落色。
const FALLBACK_COLOR: &str = "#9aa0a6";

/// 待上行的一份载荷：正文帧走可靠档，在场走瞬时档。
pub enum Outgoing {
    Frames(Value),
    Presence(Value),
}

/// 当前文档的协作状态：正文文档、在场通道与本端身份。
pub struct Collab {
    room: NoteCollab,
    presence: DocPresenceChannel,
    identity: Option<SpaceSelfIdentity>,
    /// 本地仓库没有协作房间，一切上行都不产生。
    in_space: bool,
}

impl Default for Collab {
    fn default() -> Self {
        Self::new()
    }
}

impl Collab {
    pub fn new() -> Self {
        Collab {
            room: NoteCollab::new(session_author()),
            presence: DocPresenceChannel::new(),
            identity: None,
            in_space: false,
        }
    }

    /// 房间类型与名册身份变化（进入 / 离开空间、改名换色、名册刷新）。
    pub fn set_room(&mut self, kind: SpaceContainerKind, identity: Option<SpaceSelfIdentity>) {
        let in_space = kind == SpaceContainerKind::Space;
        if self.in_space && !in_space {
            // 离开空间：协作文档与远端条目一并作废，重进空间从新基线开始
            self.room.close();
            self.presence.leave(Instant::now());
            self.presence.clear_remotes();
        }
        self.in_space = in_space;
        self.identity = identity;
    }

    /// 是否挂着协作文档：只有挂上才需要周期拍保活与剔除。
    pub fn is_active(&self) -> bool {
        self.in_space && self.presence.file().is_some()
    }

    /// 载入正文后建立基线并通告：换文件重建基线，同文件重开也会下发一份新基线通告。
    pub fn open(
        &mut self,
        file: &str,
        text: &str,
        selection: Option<(usize, usize)>,
    ) -> Vec<Outgoing> {
        if !self.in_space {
            return Vec::new();
        }
        let opened = self.room.open(file, text);
        let mut out = vec![Outgoing::Frames(frames_payload(
            file,
            vec![opened.baseline_frame, opened.sync_step1],
        ))];
        out.extend(self.enter_presence(file, text, selection));
        out
    }

    /// 本端编辑：差分进文档并上行增量，选区随行。
    pub fn edit(&mut self, text: &str, selection: Option<(usize, usize)>) -> Vec<Outgoing> {
        if !self.in_space {
            return Vec::new();
        }
        let Some(file) = self.presence.file().map(str::to_string) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let updates = self.room.local_edit(text);
        if let Some(tag) = self.room.tag().cloned() {
            let frames: Vec<Vec<u8>> = updates
                .iter()
                .map(|inner| encode_note_sync(&tag, inner))
                .collect();
            if !frames.is_empty() {
                out.push(Outgoing::Frames(frames_payload(&file, frames)));
            }
        }
        let (name, color) = self.local_identity();
        let identity = Identity { name: &name, color: &color };
        let units = selection_units(text, selection);
        let payload = self.presence.update(Instant::now(), &identity, &file, units);
        out.extend(payload.map(Outgoing::Presence));
        out
    }

    /// 选区变化（不涉正文）：点选与方向键移动要让对端可见。
    pub fn caret(&mut self, text: &str, selection: Option<(usize, usize)>) -> Vec<Outgoing> {
        if !self.in_space {
            return Vec::new();
        }
        let Some(file) = self.presence.file().map(str::to_string) else {
            return Vec::new();
        };
        let (name, color) = self.local_identity();
        let identity = Identity { name: &name, color: &color };
        let units = selection_units(text, selection);
        self.presence
            .update(Instant::now(), &identity, &file, units)
            .map(Outgoing::Presence)
            .into_iter()
            .collect()
    }

    /// 正文已落盘：推进三方合并的共同祖先。
    pub fn flushed(&mut self, file: &str, text: &str) {
        self.room.mark_disk_write(file, text);
    }

    /// 保存合流静默对齐文档：合流结果含对端已落盘内容，不得当作本端新增广播。
    pub fn aligned(&mut self, text: &str) {
        self.room.align_silent(text);
    }

    /// 入站正文帧：返回正文结果与需要随之上行的在场载荷（文档重建后重发本端身份，
    /// 让对端不画失效光标），回写正文由调用方决定。
    pub fn apply(
        &mut self,
        peer_id: &str,
        file: &str,
        frame: &[u8],
    ) -> (RemoteOutcome, Vec<Outgoing>) {
        if !self.in_space || self.presence.file() != Some(file) {
            return (RemoteOutcome::default(), Vec::new());
        }
        let outcome = self.room.apply_remote(peer_id, file, frame);
        let mut out = Vec::new();
        if outcome.rebuilt {
            let text = outcome.text.clone().unwrap_or_default();
            out.extend(self.enter_presence(file, &text, None));
        }
        (outcome, out)
    }

    /// 入站在场帧：刷新远端光标集合。
    pub fn apply_presence(&mut self, peer_id: &str, payload: &Value) -> bool {
        if !self.in_space {
            return false;
        }
        self.presence.apply(peer_id, payload, Instant::now())
    }

    /// 重连 / 反熵：重发基线与握手，并强制重发在场身份。
    pub fn resync(&mut self) -> Vec<Outgoing> {
        if !self.in_space {
            return Vec::new();
        }
        let Some(file) = self.presence.file().map(str::to_string) else {
            return Vec::new();
        };
        let mut frames = Vec::new();
        if let Some(frame) = self.room.baseline_frame() {
            frames.push(frame);
        }
        if let Some(step1) = self.room.sync_step1() {
            frames.push(step1);
        }
        let mut out = Vec::new();
        if !frames.is_empty() {
            out.push(Outgoing::Frames(frames_payload(&file, frames)));
        }
        out.extend(self.presence.resend(Instant::now()).map(Outgoing::Presence));
        out
    }

    /// 帧拍：补发被限频压下的选区、到期保活并剔除超时对端。
    pub fn tick(&mut self) -> (Vec<Outgoing>, bool) {
        if !self.is_active() {
            return (Vec::new(), false);
        }
        let now = Instant::now();
        let before = self.presence.remote().len();
        self.presence.refresh(now);
        let changed = before != self.presence.remote().len();
        let payload = self
            .presence
            .flush(now)
            .or_else(|| self.presence.keepalive_if_due(now));
        (payload.map(Outgoing::Presence).into_iter().collect(), changed)
    }

    /// 当前可绘制的远端光标（正文 UTF-8 字节偏移）。
    pub fn cursors(&self, text: &str) -> Vec<RemoteCursor> {
        if !self.in_space {
            return Vec::new();
        }
        self.presence
            .remote()
            .iter()
            .map(|caret| RemoteCursor {
                name: caret.name.clone(),
                color: caret.color.clone(),
                anchor: caret
                    .anchor
                    .map(|unit| utf16_to_byte_offset(text, unit as usize)),
                head: caret
                    .head
                    .map(|unit| utf16_to_byte_offset(text, unit as usize)),
            })
            .collect()
    }

    /// 服务端改名：正文文档与在场挂靠路径一并跟随（选区指向正文偏移，与路径无关）。
    pub fn follow_rename(&mut self, old: &str, new: &str) -> Vec<Outgoing> {
        if !self.in_space {
            return Vec::new();
        }
        self.room.relocate(old, new);
        let (name, color) = self.local_identity();
        let identity = Identity { name: &name, color: &color };
        self.presence
            .relocate(Instant::now(), &identity, old, new)
            .map(Outgoing::Presence)
            .into_iter()
            .collect()
    }

    /// 名册里消失的连接：立即摘除其光标条目（超时剔除只兜底漏帧）。
    pub fn retain_roster(&mut self, known: &[String]) -> bool {
        self.presence.retain_roster(known)
    }

    /// 关闭文档：发离场帧并销毁协作文档。
    pub fn close(&mut self) -> Vec<Outgoing> {
        let leave = self.presence.leave(Instant::now()).map(Outgoing::Presence);
        self.room.close();
        leave.into_iter().collect()
    }

    /// 进入 / 重进当前文件：无选区也上报，对端据此显示「在此文件」。
    fn enter_presence(
        &mut self,
        file: &str,
        text: &str,
        selection: Option<(usize, usize)>,
    ) -> Vec<Outgoing> {
        let (name, color) = self.local_identity();
        let identity = Identity { name: &name, color: &color };
        let units = selection_units(text, selection);
        self.presence
            .enter(Instant::now(), &identity, file, units)
            .map(Outgoing::Presence)
            .into_iter()
            .collect()
    }

    fn local_identity(&self) -> (String, String) {
        match &self.identity {
            Some(identity) => (identity.name.clone(), identity.color.clone()),
            None => (FALLBACK_NAME.to_string(), FALLBACK_COLOR.to_string()),
        }
    }
}

/// 应答帧打包：入站帧要回的帧由调用方直接上行。
pub fn frames(file: &str, frames: Vec<Vec<u8>>) -> Outgoing {
    Outgoing::Frames(frames_payload(file, frames))
}

fn frames_payload(file: &str, frames: Vec<Vec<u8>>) -> Value {
    json!({
        "file": file,
        "frames": frames
            .iter()
            .map(|frame| base64::engine::general_purpose::STANDARD.encode(frame))
            .collect::<Vec<_>>(),
    })
}

/// 入站载荷解析：返回文件路径与解码后的帧；格式不符即视为别的插件的通道数据。
pub fn decode_frames(payload: &Value) -> Option<(String, Vec<Vec<u8>>)> {
    let file = payload.get("file")?.as_str()?.to_string();
    let frames = payload.get("frames")?.as_array()?;
    let frames = frames
        .iter()
        .map(|frame| {
            let encoded = frame.as_str()?;
            base64::engine::general_purpose::STANDARD.decode(encoded).ok()
        })
        .collect::<Option<Vec<_>>>()?;
    Some((file, frames))
}

fn selection_units(text: &str, selection: Option<(usize, usize)>) -> Option<(u32, u32)> {
    let (anchor, head) = selection?;
    let unit = |byte: usize| u32::try_from(byte_to_utf16_offset(text, byte.min(text.len()))).ok();
    Some((unit(anchor)?, unit(head)?))
}

fn session_author() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}-{:x}", std::process::id())
}
