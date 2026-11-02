//! 房间状态：宿主给的空间快照（容器 / 连接态 / 本端身份 / 在线设备）与成员名册快照。
//! 同一身份的多条连接在这一层合并成一行；插件不连服务端、不持名册真源。

use atelyx_plugin_protocol::method::{
    Radii, SpaceContainerKind, SpaceMember, SpaceMembersResult, SpacePeer, SpacePeerFile,
    SpaceRoomResult,
};

/// 徽标与成员色取宿主主题：强调色 / 次级字色 / 弱化字色 + 圆角档。
#[derive(Clone, Debug)]
pub struct Palette {
    pub accent: String,
    pub text_secondary: String,
    pub text_muted: String,
    pub radii: Radii,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            accent: "#4a90d9".into(),
            text_secondary: "#57606a".into(),
            text_muted: "#8c959f".into(),
            radii: Radii::default(),
        }
    }
}

/// 房间展示的一行在线设备（本端一行 + 每个「昵称 + 设备」一行）。
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    pub nickname: String,
    /// 设备名（合并键的一部分，不直接展示）。
    pub device: String,
    /// 成员色（`#rrggbb`）。
    pub color: String,
    pub platform: Option<String>,
    pub version: Option<String>,
    pub files: Vec<SpacePeerFile>,
    pub is_self: bool,
}

/// 面板可见状态（每帧克隆一份给渲染，避免持有锁跨 await）。
#[derive(Clone)]
pub struct Model {
    /// `None` = 首份回执未到：不画空态，避免开面板瞬间闪出「未进入协作空间」。
    pub room: Option<SpaceRoomResult>,
    /// 房间快照读取失败的原因（手上已有快照时只给提示行，不清空已看着的内容）。
    pub room_error: Option<String>,
    pub members: SpaceMembersResult,
    pub palette: Palette,
    /// 快照世代：行命中区 id 带着它，过期帧上的点击据此作废。
    pub gen: u64,
}

impl Model {
    pub fn new() -> Self {
        Self {
            room: None,
            room_error: None,
            members: SpaceMembersResult::default(),
            palette: Palette::default(),
            gen: 0,
        }
    }

    pub fn apply_room(&mut self, room: SpaceRoomResult) {
        self.room = Some(room);
        self.room_error = None;
        self.gen = self.gen.wrapping_add(1);
    }

    /// 记一次房间快照读取失败：保留已有快照，空快照时由视图给可读原因与重试。
    pub fn fail_room(&mut self, error: String) {
        self.room_error = Some(error);
        self.gen = self.gen.wrapping_add(1);
    }

    pub fn clear_room_error(&mut self) {
        self.room_error = None;
    }

    pub fn apply_members(&mut self, members: SpaceMembersResult) {
        self.members = members;
        self.gen = self.gen.wrapping_add(1);
    }

    /// 当前容器是协作空间（本地仓库只出空态）。
    pub fn in_space(&self) -> bool {
        self.room
            .as_ref()
            .is_some_and(|room| room.kind == SpaceContainerKind::Space)
    }

    /// 在线台数：本端 + 全部对端连接（合并前计数，与连接数一致）。
    pub fn online_count(&self) -> usize {
        self.room
            .as_ref()
            .map(|room| room.peers.len() + 1)
            .unwrap_or(1)
    }

    pub fn connected(&self) -> bool {
        self.room.as_ref().is_some_and(|room| room.connected)
    }

    /// 名册里的成员（按宿主给的顺序）。
    pub fn members(&self) -> &[SpaceMember] {
        &self.members.members
    }

    /// 设备行：本端在前，其余按「昵称 + 设备」合并（同身份多连接合成一行）。
    pub fn devices(&self) -> Vec<Device> {
        let Some(room) = self.room.as_ref() else { return Vec::new() };
        let mut rows = Vec::new();
        if let Some(identity) = room.self_identity.as_ref() {
            rows.push(Device {
                nickname: identity.name.clone(),
                device: String::new(),
                color: identity.color.clone(),
                platform: Some(identity.platform.clone()),
                version: Some(identity.version.clone()),
                files: identity.files.clone(),
                is_self: true,
            });
        }
        rows.extend(merge_peers(&room.peers));
        rows
    }
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

/// 按「昵称 + 设备」合并对端连接：打开文件取并集去重，色取首条，平台 / 版本取首个已知值。
fn merge_peers(peers: &[SpacePeer]) -> Vec<Device> {
    let mut rows: Vec<Device> = Vec::new();
    for peer in peers {
        match rows
            .iter_mut()
            .find(|row| !row.is_self && row.nickname == peer.nickname && row.device == peer.device)
        {
            Some(row) => {
                for file in &peer.files {
                    if !row.files.iter().any(|existing| existing.path == file.path) {
                        row.files.push(file.clone());
                    }
                }
                if row.platform.is_none() {
                    row.platform = peer.platform.clone();
                }
                if row.version.is_none() {
                    row.version = peer.version.clone();
                }
            }
            None => rows.push(Device {
                nickname: peer.nickname.clone(),
                device: peer.device.clone(),
                color: peer.color.clone(),
                platform: peer.platform.clone(),
                version: peer.version.clone(),
                files: peer.files.clone(),
                is_self: false,
            }),
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use atelyx_plugin_protocol::method::{SpaceMemberRole, SpaceSelfIdentity};

    fn file(path: &str) -> SpacePeerFile {
        SpacePeerFile { path: path.into(), icon: "sticky-note".into() }
    }

    fn peer(id: &str, nickname: &str, device: &str, files: Vec<SpacePeerFile>) -> SpacePeer {
        SpacePeer {
            peer_id: id.into(),
            nickname: nickname.into(),
            device: device.into(),
            color: "#e06c75".into(),
            platform: Some("windows".into()),
            version: Some("0.6.5".into()),
            files,
        }
    }

    fn room(peers: Vec<SpacePeer>) -> SpaceRoomResult {
        SpaceRoomResult {
            kind: SpaceContainerKind::Space,
            connected: true,
            self_peer_id: Some("1".into()),
            self_identity: Some(SpaceSelfIdentity {
                name: "我".into(),
                color: "#30bced".into(),
                platform: "linux".into(),
                version: "0.6.5".into(),
                files: vec![file("a.md")],
            }),
            peers,
        }
    }

    /// 本端恒在第一行；同「昵称 + 设备」的连接合并成一行，打开文件并集去重。
    #[test]
    fn devices_merge_same_identity_connections() {
        let mut model = Model::new();
        model.apply_room(room(vec![
            peer("2", "张三", "桌面端", vec![file("a.md"), file("b.md")]),
            peer("3", "张三", "桌面端", vec![file("b.md"), file("c.md")]),
            peer("4", "李四", "笔记本", vec![file("d.md")]),
        ]));
        let devices = model.devices();
        assert_eq!(devices.len(), 3, "本端 + 两个身份");
        assert!(devices[0].is_self);
        assert_eq!(devices[1].nickname, "张三");
        assert_eq!(
            devices[1].files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
            ["a.md", "b.md", "c.md"]
        );
        assert_eq!(devices[2].nickname, "李四");
    }

    /// 平台 / 版本取首个已知值（后到的缺省不覆盖既有值）。
    #[test]
    fn devices_keep_first_known_platform_and_version() {
        let mut model = Model::new();
        let mut unknown = peer("3", "张三", "桌面端", Vec::new());
        unknown.platform = None;
        unknown.version = None;
        model.apply_room(room(vec![peer("2", "张三", "桌面端", Vec::new()), unknown]));
        let devices = model.devices();
        assert_eq!(devices.len(), 2);
        assert_eq!(devices[1].platform.as_deref(), Some("windows"));
        assert_eq!(devices[1].version.as_deref(), Some("0.6.5"));
    }

    /// 本地仓库不出设备行；在线台数 = 对端连接数 + 1。
    #[test]
    fn local_container_has_no_devices() {
        let mut model = Model::new();
        model.apply_room(SpaceRoomResult::default());
        assert!(!model.in_space());
        assert!(model.devices().is_empty());
        assert_eq!(model.online_count(), 1);
    }

    #[test]
    fn members_snapshot_is_a_passthrough() {
        let mut model = Model::new();
        model.apply_members(SpaceMembersResult {
            ready: true,
            loading: false,
            error: None,
            members: vec![SpaceMember {
                username: "owner".into(),
                display_name: "所有者".into(),
                role: SpaceMemberRole::Owner,
            }],
        });
        assert_eq!(model.members().len(), 1);
    }
}
