//! 服务端集成测试（验收口径）：两个账号共享一个空间；非成员请求被拒；反链 / 标签 / 搜索在空间内容上可用。
//!
//! 每个用例独立临时数据目录 + 真实端口，进程内起服务器，用真实 HTTP / WS 客户端完整打请求链路，
//! 不 mock 路由与鉴权层。

use std::collections::HashSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use futures_util::future::join_all;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

/// 限额 override（set_size_limit_override）是进程级全局：并行测试互踩（如 1MB 大 value 写入
/// 撞上 1KB 测试上限）。涉及 override 与大 value 的测试都持这把锁串行执行。
static TEST_SERIAL: Mutex<()> = Mutex::new(());

/// 限额 override 的作用域守卫：构造即设置、drop 即复位，panic 路径也不残留脏限额。
struct SizeLimitOverride;
impl SizeLimitOverride {
    fn set(file: Option<usize>, meta: Option<usize>) -> Self {
        collab_relay::set_size_limit_override(file, meta);
        Self
    }
}
impl Drop for SizeLimitOverride {
    fn drop(&mut self) {
        collab_relay::set_size_limit_override(None, None);
    }
}

// ===== 脚手架 =====

/// 起一个服务器实例（独立数据目录 + 临时端口），返回 base URL。
async fn spawn_server(data_dir: &Path) -> String {
    let state = collab_relay::ServerState::open(data_dir);
    let app = collab_relay::build_app(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("绑定临时端口");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        collab_relay::serve_on(listener, app, None)
            .await
            .expect("服务器错误");
    });
    format!("http://{addr}")
}

struct Ctx {
    http: reqwest::Client,
    base: String,
}

impl Ctx {
    fn new(base: String) -> Self {
        Self { http: reqwest::Client::new(), base }
    }

    async fn send(
        &self,
        method: reqwest::Method,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
        query: &[(&str, &str)],
    ) -> (u16, Value) {
        let mut req = self.http.request(method, format!("{}{path}", self.base));
        if let Some(t) = token {
            req = req.bearer_auth(t);
        }
        if let Some(b) = body {
            req = req.json(&b);
        }
        if !query.is_empty() {
            req = req.query(query);
        }
        let resp = req.send().await.expect("请求失败");
        let status = resp.status().as_u16();
        let text = resp.text().await.unwrap_or_default();
        let json = if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::Null) };
        (status, json)
    }

    async fn get(&self, path: &str, token: Option<&str>, query: &[(&str, &str)]) -> (u16, Value) {
        self.send(reqwest::Method::GET, path, token, None, query).await
    }
    async fn post(&self, path: &str, token: Option<&str>, body: Value) -> (u16, Value) {
        self.send(reqwest::Method::POST, path, token, Some(body), &[]).await
    }
    async fn put(&self, path: &str, token: Option<&str>, body: Value) -> (u16, Value) {
        self.send(reqwest::Method::PUT, path, token, Some(body), &[]).await
    }
    async fn delete(&self, path: &str, token: Option<&str>) -> (u16, Value) {
        self.send(reqwest::Method::DELETE, path, token, None, &[]).await
    }
}

/// 注册并返回令牌（用户名唯一由调用方保证）。
async fn register(ctx: &Ctx, username: &str) -> String {
    let (status, body) = ctx
        .post(
            "/api/auth/register",
            None,
            json!({ "username": username, "password": "pass-123456" }),
        )
        .await;
    assert_eq!(status, 200, "注册应成功：{body}");
    body["token"].as_str().expect("注册应返回令牌").to_string()
}

/// A 建空间 + 给 B 签发 editor 邀请码，B 接受。返回 spaceId。
async fn setup_two_members(ctx: &Ctx, a: &str, b: &str) -> String {
    let a_token = register(ctx, a).await;
    let (_, body) = ctx.post("/api/spaces", Some(&a_token), json!({ "name": "项目空间" })).await;
    let space_id = body["spaceId"].as_str().unwrap().to_string();
    let (_, invite) = ctx
        .post(&format!("/api/spaces/{space_id}/invites"), Some(&a_token), json!({ "role": "editor" }))
        .await;
    let code = invite["code"].as_str().unwrap().to_string();
    let b_token = register(ctx, b).await;
    let (status, accepted) = ctx.post("/api/invites/accept", Some(&b_token), json!({ "code": code })).await;
    assert_eq!(status, 200, "接受邀请应成功：{accepted}");
    assert_eq!(accepted["role"], "editor");
    space_id
}

// ===== 账号与会话 =====

#[tokio::test]
async fn register_login_and_session_management() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);

    let (status, body) = ctx
        .post("/api/auth/register", None, json!({ "username": "alice", "password": "pass-123456", "displayName": "爱丽丝" }))
        .await;
    assert_eq!(status, 200);
    let first_token = body["token"].as_str().unwrap().to_string();
    assert_eq!(body["displayName"], "爱丽丝");

    // 用户名重复 → 409；密码错误 → 401；非法用户名 → 400
    let (status, _) = ctx.post("/api/auth/register", None, json!({ "username": "alice", "password": "x-pass-123" })).await;
    assert_eq!(status, 409);
    let (status, _) = ctx.post("/api/auth/login", None, json!({ "username": "alice", "password": "wrong-pass-1" })).await;
    assert_eq!(status, 401);
    let (status, _) = ctx.post("/api/auth/register", None, json!({ "username": "a b", "password": "x-pass-123" })).await;
    assert_eq!(status, 400);

    // 二次登录 = 新设备会话，两枚令牌并存
    let (status, login) = ctx
        .post("/api/auth/login", None, json!({ "username": "alice", "password": "pass-123456", "deviceName": "台式机" }))
        .await;
    assert_eq!(status, 200);
    let second_token = login["token"].as_str().unwrap().to_string();
    assert_ne!(first_token, second_token);

    // 设备列表：两枚会话，当前会话有标记
    let (status, devices) = ctx.get("/api/auth/devices", Some(&second_token), &[]).await;
    assert_eq!(status, 200);
    let list = devices.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert!(list.iter().any(|d| d["deviceName"] == "台式机" && d["current"] == true));

    // 吊销另一枚会话后其令牌失效；未认证 / 伪令牌一律 401
    let other_id = list.iter().find(|d| d["current"] == false).unwrap()["id"].as_str().unwrap().to_string();
    let (status, _) = ctx.delete(&format!("/api/auth/devices/{other_id}"), Some(&second_token)).await;
    assert_eq!(status, 200);
    let (status, _) = ctx.get("/api/auth/devices", Some(&first_token), &[]).await;
    assert_eq!(status, 401);
    let (status, _) = ctx.get("/api/auth/devices", None, &[]).await;
    assert_eq!(status, 401);
    let (status, _) = ctx.get("/api/auth/devices", Some("not-a-token"), &[]).await;
    assert_eq!(status, 401);

    // 登出后当前令牌失效
    let (status, _) = ctx.post("/api/auth/logout", Some(&second_token), json!({})).await;
    assert_eq!(status, 200);
    let (status, _) = ctx.get("/api/auth/devices", Some(&second_token), &[]).await;
    assert_eq!(status, 401);
}

// ===== 两账号共享空间 + 内容 CRUD =====

#[tokio::test]
async fn two_accounts_share_space_with_content_ops() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a_token = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "alice", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };
    let b_token = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "bob", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };

    // 成员列表：owner + editor
    let (status, members) = ctx.get(&format!("/api/spaces/{space_id}/members"), Some(&a_token), &[]).await;
    assert_eq!(status, 200);
    assert_eq!(members.as_array().unwrap().len(), 2);
    assert!(members.as_array().unwrap().iter().any(|m| m["role"] == "owner"));
    assert!(members.as_array().unwrap().iter().any(|m| m["role"] == "editor"));

    // B 写、A 读；A 写、B 读（同一份内容，真源在服务端）
    let (status, w) = ctx
        .put(&format!("/api/spaces/{space_id}/file"), Some(&b_token), json!({ "path": "笔记/方案.md", "content": "# 方案\n正文" }))
        .await;
    assert_eq!(status, 200, "editor 写入应成功：{w}");
    assert!(w["updatedAt"].is_number());
    let (status, r) = ctx.get(&format!("/api/spaces/{space_id}/file"), Some(&a_token), &[("path", "笔记/方案.md")]).await;
    assert_eq!(status, 200);
    assert_eq!(r["content"], "# 方案\n正文");

    let (status, _) = ctx
        .put(&format!("/api/spaces/{space_id}/file"), Some(&a_token), json!({ "path": "笔记/方案.md", "content": "# 方案\n改过了" }))
        .await;
    assert_eq!(status, 200);
    let (_, r) = ctx.get(&format!("/api/spaces/{space_id}/file"), Some(&b_token), &[("path", "笔记/方案.md")]).await;
    assert_eq!(r["content"], "# 方案\n改过了");

    // 树：嵌套结构 + isDir + updatedAt
    let (status, tree) = ctx.get(&format!("/api/spaces/{space_id}/tree"), Some(&b_token), &[]).await;
    assert_eq!(status, 200);
    let nodes = tree.as_array().unwrap();
    let dir_node = nodes.iter().find(|n| n["name"] == "笔记").expect("树应含 笔记 目录");
    assert_eq!(dir_node["isDir"], true);
    assert_eq!(dir_node["children"].as_array().unwrap().len(), 1);

    // 建文件夹 / 复制 / 重命名（跨目录移动）/ 删除
    let (status, _) = ctx.post(&format!("/api/spaces/{space_id}/folder"), Some(&b_token), json!({ "path": "归档" })).await;
    assert_eq!(status, 200);
    let (status, _) = ctx.post(&format!("/api/spaces/{space_id}/copy"), Some(&b_token), json!({ "fromPath": "笔记/方案.md", "toPath": "归档/方案副本.md" })).await;
    assert_eq!(status, 200);
    let (status, _) = ctx.post(&format!("/api/spaces/{space_id}/rename"), Some(&a_token), json!({ "oldPath": "归档/方案副本.md", "newPath": "归档/旧方案.md" })).await;
    assert_eq!(status, 200);
    let (status, _) = ctx.get(&format!("/api/spaces/{space_id}/file"), Some(&a_token), &[("path", "归档/旧方案.md")]).await;
    assert_eq!(status, 200);

    // 删除非空文件夹：先回 needsConfirm，force 后删除
    let (status, del) = ctx
        .send(reqwest::Method::DELETE, &format!("/api/spaces/{space_id}/folder"), Some(&a_token), Some(json!({ "path": "归档" })), &[])
        .await;
    assert_eq!(status, 200);
    assert_eq!(del["needsConfirm"], true);
    let (status, del) = ctx
        .send(reqwest::Method::DELETE, &format!("/api/spaces/{space_id}/folder"), Some(&a_token), Some(json!({ "path": "归档", "force": true })), &[])
        .await;
    assert_eq!(status, 200);
    assert_eq!(del["deleted"], true);
    let (status, _) = ctx.get(&format!("/api/spaces/{space_id}/file"), Some(&a_token), &[("path", "归档/旧方案.md")]).await;
    assert_eq!(status, 404);

    // 删除文件
    let (status, _) = ctx
        .send(reqwest::Method::DELETE, &format!("/api/spaces/{space_id}/file"), Some(&b_token), Some(json!({})), &[("path", "笔记/方案.md")])
        .await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn path_traversal_and_invalid_paths_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let a_token = register(&ctx, "alice").await;
    let (_, body) = ctx.post("/api/spaces", Some(&a_token), json!({ "name": "s" })).await;
    let space_id = body["spaceId"].as_str().unwrap().to_string();

    for bad in ["../escape.md", "a/../../escape.md", "/abs.md", "C:\\x.md", ""] {
        let (status, _) = ctx
            .put(&format!("/api/spaces/{space_id}/file"), Some(&a_token), json!({ "path": bad, "content": "x" }))
            .await;
        assert_eq!(status, 400, "穿越路径应被拒：{bad}");
        let (status, _) = ctx.get(&format!("/api/spaces/{space_id}/file"), Some(&a_token), &[("path", bad)]).await;
        assert_eq!(status, 400, "穿越路径读应被拒：{bad}");
    }
    // 写入成功后服务器数据目录外不应出现逃逸文件
    assert!(!dir.path().join("escape.md").exists());
    assert!(!dir.path().parent().unwrap().join("escape.md").exists());
}

// ===== 权限边界 =====

#[tokio::test]
async fn non_member_and_insufficient_role_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a_token = register(&ctx, "carol").await; // carol 未加入
    let b_token = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "bob", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };

    // 非成员摸内容全被拒（403）；未知空间 404
    let (status, _) = ctx.get(&format!("/api/spaces/{space_id}/tree"), Some(&a_token), &[]).await;
    assert_eq!(status, 403);
    let (status, _) = ctx.get("/api/spaces/unknown-id/tree", Some(&a_token), &[]).await;
    assert_eq!(status, 404);
    let (status, _) = ctx
        .put(&format!("/api/spaces/{space_id}/file"), Some(&a_token), json!({ "path": "x.md", "content": "x" }))
        .await;
    assert_eq!(status, 403);
    let (status, _) = ctx.get(&format!("/api/spaces/{space_id}/members"), Some(&a_token), &[]).await;
    assert_eq!(status, 403);

    // 可见性隔离：carol 的空间列表不含别人的空间
    let (status, spaces) = ctx.get("/api/spaces", Some(&a_token), &[]).await;
    assert_eq!(status, 200);
    assert!(spaces.as_array().unwrap().is_empty());

    // editor 的 owner-only 端点全被拒
    let (status, _) = ctx.send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}"), Some(&b_token), Some(json!({ "name": "改名" })), &[]).await;
    assert_eq!(status, 403, "editor 不可改名");
    let (status, _) = ctx.post(&format!("/api/spaces/{space_id}/invites"), Some(&b_token), json!({ "role": "editor" })).await;
    assert_eq!(status, 403, "editor 不可签发邀请");
    let (status, _) = ctx.post(&format!("/api/spaces/{space_id}/transfer"), Some(&b_token), json!({ "toUserId": "x" })).await;
    assert_eq!(status, 403, "editor 不可转让");
    let (status, _) = ctx
        .send(reqwest::Method::DELETE, &format!("/api/spaces/{space_id}/members/whoever"), Some(&b_token), None, &[])
        .await;
    assert_eq!(status, 403, "editor 不可移除成员");
    // editor 可以看成员列表
    let (status, _) = ctx.get(&format!("/api/spaces/{space_id}/members"), Some(&b_token), &[]).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn invite_expiry_max_uses_and_revocation() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let a_token = register(&ctx, "alice").await;
    let (_, body) = ctx.post("/api/spaces", Some(&a_token), json!({ "name": "s" })).await;
    let space_id = body["spaceId"].as_str().unwrap().to_string();

    // 过期邀请 → 410
    let (_, invite) = ctx
        .post(&format!("/api/spaces/{space_id}/invites"), Some(&a_token), json!({ "role": "editor", "expiresInHours": -1 }))
        .await;
    let expired = invite["code"].as_str().unwrap().to_string();
    let c_token = register(&ctx, "carol").await;
    let (status, _) = ctx.post("/api/invites/accept", Some(&c_token), json!({ "code": expired })).await;
    assert_eq!(status, 410);

    // 次数用尽 → 409
    let (_, invite) = ctx
        .post(&format!("/api/spaces/{space_id}/invites"), Some(&a_token), json!({ "role": "editor", "maxUses": 1 }))
        .await;
    let once = invite["code"].as_str().unwrap().to_string();
    let (status, _) = ctx.post("/api/invites/accept", Some(&c_token), json!({ "code": once })).await;
    assert_eq!(status, 200);
    let d_token = register(&ctx, "dave").await;
    let (status, _) = ctx.post("/api/invites/accept", Some(&d_token), json!({ "code": once })).await;
    assert_eq!(status, 409);

    // 吊销后 → 404；未知码 → 404；非法角色 → 400
    let (_, invite) = ctx.post(&format!("/api/spaces/{space_id}/invites"), Some(&a_token), json!({ "role": "editor" })).await;
    let code = invite["code"].as_str().unwrap().to_string();
    let (status, _) = ctx.delete(&format!("/api/spaces/{space_id}/invites/{code}"), Some(&a_token)).await;
    assert_eq!(status, 200);
    let (status, _) = ctx.post("/api/invites/accept", Some(&d_token), json!({ "code": code })).await;
    assert_eq!(status, 404);
    let (status, _) = ctx.post("/api/invites/accept", Some(&d_token), json!({ "code": "no-such-code" })).await;
    assert_eq!(status, 404);
    let (status, invite) = ctx.post(&format!("/api/spaces/{space_id}/invites"), Some(&a_token), json!({ "role": "viewer" })).await;
    assert_eq!(status, 200, "viewer 角色可签发邀请");
    assert_eq!(invite["role"], "viewer");
}

#[tokio::test]
async fn transfer_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a_token = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "alice", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };
    let (status, members) = ctx.get(&format!("/api/spaces/{space_id}/members"), Some(&a_token), &[]).await;
    assert_eq!(status, 200);
    let bob_id = members.as_array().unwrap().iter().find(|m| m["username"] == "bob").unwrap()["userId"]
        .as_str()
        .unwrap()
        .to_string();
    let b_token = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "bob", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };

    // 转让给非成员 → 400
    let (status, _) = ctx.post(&format!("/api/spaces/{space_id}/transfer"), Some(&a_token), json!({ "toUserId": "nobody" })).await;
    assert_eq!(status, 400);

    // 转让后：B 是 owner（可改名/邀请），A 降为 editor（不可再邀请，但可读写内容）
    let (status, _) = ctx.post(&format!("/api/spaces/{space_id}/transfer"), Some(&a_token), json!({ "toUserId": bob_id })).await;
    assert_eq!(status, 200);
    let (status, _) = ctx.send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}"), Some(&b_token), Some(json!({ "name": "新名字" })), &[]).await;
    assert_eq!(status, 200, "新 owner 可改名");
    let (status, _) = ctx.post(&format!("/api/spaces/{space_id}/invites"), Some(&b_token), json!({ "role": "editor" })).await;
    assert_eq!(status, 200, "新 owner 可签发邀请");
    let (status, _) = ctx.post(&format!("/api/spaces/{space_id}/invites"), Some(&a_token), json!({ "role": "editor" })).await;
    assert_eq!(status, 403, "原 owner 降为 editor 后不可签发邀请");
    let (status, _) = ctx
        .put(&format!("/api/spaces/{space_id}/file"), Some(&a_token), json!({ "path": "x.md", "content": "x" }))
        .await;
    assert_eq!(status, 200, "降级后仍可读写内容");
}

// ===== 派生索引与扫描 =====

#[tokio::test]
async fn derived_indexes_on_space_content() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a_token = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "alice", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };

    let put = |path: String, content: String| {
        let ctx = &ctx;
        let token = a_token.clone();
        let space = space_id.clone();
        async move { ctx.put(&format!("/api/spaces/{space}/file"), Some(&token), json!({ "path": path, "content": content })).await }
    };
    let (status, _) = put("笔记/会议.md".into(), "---\ntags: [工作, 休息]\n---\n# 会议纪要\n见 [[记录]] 与 [文本](笔记/备忘.md)\n#生活\n".into()).await;
    assert_eq!(status, 200);
    let (status, _) = put("笔记/记录.md".into(), "# 记录\n#工作 进行中\n```\n#代码块内不算\n```\n行内代码 `#内联不算` 不算\n#2024 不算\n".into()).await;
    assert_eq!(status, 200);
    let (status, _) = put("笔记/备忘.md".into(), "# 备忘\n#工作 收尾\n".into()).await;
    assert_eq!(status, 200);
    let (status, _) = put(".隐藏/secret.md".into(), "#秘密 不进索引\n".into()).await;
    assert_eq!(status, 200);
    let (status, _) = put("画布.atlx".into(), "{}".into()).await;
    assert_eq!(status, 200);

    // 反链：`[[记录]]` 与 `[文本](笔记/备忘.md)` 两种写法都命中 会议.md
    let (status, rows) = ctx
        .get(&format!("/api/spaces/{space_id}/backlinks"), Some(&a_token), &[("noteName", "记录"), ("noteFile", "笔记/记录.md")])
        .await;
    assert_eq!(status, 200);
    assert!(rows.as_array().unwrap().iter().any(|r| r["file"] == "笔记/会议.md"), "反链应含 会议.md：{rows}");
    let (status, rows) = ctx
        .get(&format!("/api/spaces/{space_id}/backlinks"), Some(&a_token), &[("noteName", "备忘"), ("noteFile", "笔记/备忘.md")])
        .await;
    assert_eq!(status, 200);
    assert!(rows.as_array().unwrap().iter().any(|r| r["file"] == "笔记/会议.md"), "路径式链接应命中反链：{rows}");

    // 标签：frontmatter + 内联合并计数；代码块/行内代码/纯数字不算；隐藏目录不算
    let (status, tags) = ctx.get(&format!("/api/spaces/{space_id}/tags"), Some(&a_token), &[]).await;
    assert_eq!(status, 200);
    let find = |name: &str| tags.as_array().unwrap().iter().find(|t| t["tag"] == name).map(|t| t["count"].as_u64().unwrap());
    assert_eq!(find("工作"), Some(3), "工作 = 会议(frontmatter) + 记录 + 备忘：{tags}");
    assert_eq!(find("休息"), Some(1));
    assert_eq!(find("生活"), Some(1));
    assert_eq!(find("秘密"), None, "隐藏目录不进索引");
    assert!(find("2024").is_none(), "纯数字不算标签");

    // glob：任意深度命中 .md；.atlx 不算（glob 只认模式）；隐藏目录排除
    let (status, g) = ctx.post(&format!("/api/spaces/{space_id}/glob"), Some(&a_token), json!({ "pattern": "*.md" })).await;
    assert_eq!(status, 200);
    assert_eq!(g["total"], 3, "*.md 应命中三篇笔记：{g}");
    assert!(!g["paths"].as_array().unwrap().iter().any(|p| p.as_str().unwrap().contains("secret")));

    // grep：正则 + 行号；include 过滤
    let (status, grep) = ctx
        .post(&format!("/api/spaces/{space_id}/grep"), Some(&a_token), json!({ "pattern": "工作", "include": "*.md" }))
        .await;
    assert_eq!(status, 200);
    let matches = grep["matches"].as_array().unwrap();
    assert!(matches.iter().any(|m| m["path"] == "笔记/记录.md" && m["lineNumber"] == 2), "{grep}");
    assert!(matches.iter().any(|m| m["path"] == "笔记/备忘.md"), "{grep}");
}

// ===== WS 空间频道 =====

type WsStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn ws_connect(base: &str, path: &str, hello: Value) -> WsStream {
    let url = format!("{}{path}", base.replacen("http", "ws", 1));
    let (mut ws, _) = tokio_tungstenite::connect_async(&url).await.expect("WS 连接失败");
    ws.send(Message::text(hello.to_string())).await.expect("发送 hello 失败");
    ws
}

async fn next_frame(ws: &mut WsStream) -> Value {
    let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("等待帧超时")
        .expect("流结束")
        .expect("帧错误");
    serde_json::from_str(&frame.to_string()).expect("帧须为 JSON")
}

#[tokio::test]
async fn ws_space_channel_auth_and_presence() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a_token = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "alice", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };
    let b_token = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "bob", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };
    let c_token = register(&ctx, "carol").await;

    // 两个成员入频道：各自收到 hello-ack，且 peers 快照含双方
    let mut a = ws_connect(&ctx.base, "/ws/space", json!({ "type": "hello", "spaceId": space_id, "token": a_token, "nickname": "爱丽丝", "color": "#ff0000", "deviceName": "A机" })).await;
    let ack = next_frame(&mut a).await;
    assert_eq!(ack["type"], "hello-ack");
    let mut b = ws_connect(&ctx.base, "/ws/space", json!({ "type": "hello", "spaceId": space_id, "token": b_token, "nickname": "鲍勃", "color": "#00ff00", "deviceName": "B机" })).await;
    let ack_b = next_frame(&mut b).await;
    assert_eq!(ack_b["type"], "hello-ack");
    // peers 快照广播给全员（含自己）：A 收过 size1（自己入房）后，等 size2（B 入房）
    let mut peers = next_frame(&mut a).await;
    for _ in 0..3 {
        if peers["type"] == "peers" && peers["peers"].as_array().unwrap().len() == 2 {
            break;
        }
        peers = next_frame(&mut a).await;
    }
    assert_eq!(peers["peers"].as_array().unwrap().len(), 2);

    // presence 转发：A 发，B 收（不含自己）。B 先 drain 掉自己入房触发的 peers 快照
    a.send(Message::text(json!({ "type": "presence", "file": "笔记/会议.md" }).to_string())).await.unwrap();
    let mut presence = next_frame(&mut b).await;
    for _ in 0..3 {
        if presence["type"] == "presence" {
            break;
        }
        presence = next_frame(&mut b).await;
    }
    assert_eq!(presence["type"], "presence");
    assert_eq!(presence["presence"]["file"], "笔记/会议.md");

    // 非成员与伪令牌：error 帧后连接被关闭
    let mut c = ws_connect(&ctx.base, "/ws/space", json!({ "type": "hello", "spaceId": space_id, "token": c_token, "nickname": "c" })).await;
    let err = next_frame(&mut c).await;
    assert_eq!(err["type"], "error");
    // 服务端回 error 帧后关闭连接：读到 Close 帧或流结束都算断开
    let mut closed = false;
    for _ in 0..3 {
        match tokio::time::timeout(Duration::from_secs(5), c.next()).await {
            Ok(None) => { closed = true; break; }
            Ok(Some(Ok(Message::Close(_)))) => { closed = true; break; }
            Ok(Some(Ok(_))) => continue,
            // 传输层错误（服务端 drop socket 的硬断）也算断开；只有超时才是「仍连着」
            Ok(Some(Err(_))) => { closed = true; break; }
            Err(_) => break,
        }
    }
    assert!(closed, "非成员应被断开");
    let mut fake = ws_connect(&ctx.base, "/ws/space", json!({ "type": "hello", "spaceId": space_id, "token": "bogus", "nickname": "x" })).await;
    let err = next_frame(&mut fake).await;
    assert_eq!(err["type"], "error");
}

// ===== 持久化 =====

#[tokio::test]
async fn state_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let base = spawn_server(dir.path()).await;
    let ctx = Ctx::new(base);
    let (status, reg) = ctx.post("/api/auth/register", None, json!({ "username": "alice", "password": "pass-123456" })).await;
    assert_eq!(status, 200);
    let token = reg["token"].as_str().unwrap().to_string();
    let (_, body) = ctx.post("/api/spaces", Some(&token), json!({ "name": "长期空间" })).await;
    let space_id = body["spaceId"].as_str().unwrap().to_string();
    let (status, _) = ctx.put(&format!("/api/spaces/{space_id}/file"), Some(&token), json!({ "path": "a.md", "content": "重启后仍在" })).await;
    assert_eq!(status, 200);

    // 同一数据目录起第二个实例：账号可登录、令牌仍有效、内容仍在
    let ctx2 = Ctx::new(spawn_server(dir.path()).await);
    let (status, _) = ctx2.post("/api/auth/login", None, json!({ "username": "alice", "password": "pass-123456" })).await;
    assert_eq!(status, 200, "重启后账号仍可登录");
    let (status, spaces) = ctx2.get("/api/spaces", Some(&token), &[]).await;
    assert_eq!(status, 200, "重启后会话令牌仍有效");
    assert_eq!(spaces.as_array().unwrap().len(), 1);
    let (status, file) = ctx2.get(&format!("/api/spaces/{space_id}/file"), Some(&token), &[("path", "a.md")]).await;
    assert_eq!(status, 200);
    assert_eq!(file["content"], "重启后仍在");
}

// ===== 可选 TLS =====

#[tokio::test]
async fn tls_enabled_with_cert_paths() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    // 自签证书（部署方提供正式证书；此处只验证 TLS 链路本身可用）
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        .expect("生成自签证书失败");
    let cert_path = dir.path().join("cert.pem");
    let key_path = dir.path().join("key.pem");
    std::fs::write(&cert_path, cert.cert.pem()).unwrap();
    std::fs::write(&key_path, cert.key_pair.serialize_pem()).unwrap();

    let state = collab_relay::ServerState::open(&data_dir);
    let app = collab_relay::build_app(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        collab_relay::serve_on(
            listener,
            app,
            Some(collab_relay::TlsPaths { cert: cert_path, key: key_path }),
        )
        .await
        .expect("服务器错误");
    });

    let http = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap();
    let resp = http
        .post(format!("https://{addr}/api/auth/register"))
        .json(&json!({ "username": "alice", "password": "pass-123456" }))
        .send()
        .await
        .expect("TLS 请求失败");
    assert_eq!(resp.status().as_u16(), 200, "TLS 链路上注册应成功");
}

// ===== 收编既有目录 =====

#[tokio::test]
async fn adopt_existing_folder_as_space() {
    let dir = tempfile::tempdir().unwrap();
    // 模拟 NAS 上已按仓库结构组织的文件夹（含遗留 .atelyx 与旧文件）
    let vault = tempfile::tempdir().unwrap();
    let vault_root = dunce::canonicalize(vault.path()).unwrap();
    std::fs::create_dir_all(vault_root.join("笔记")).unwrap();
    std::fs::create_dir_all(vault_root.join(".atelyx")).unwrap();
    std::fs::write(vault_root.join("笔记/旧笔记.md"), "# 旧笔记\n#收编内容\n").unwrap();
    std::fs::write(vault_root.join("画布.atlx"), "{}").unwrap();
    std::fs::write(vault_root.join(".atelyx/config.json"), "{}").unwrap();

    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let a_token = register(&ctx, "alice").await;
    let vault_str = vault_root.to_string_lossy().to_string();
    let (status, body) = ctx
        .post("/api/spaces", Some(&a_token), json!({ "name": "收编空间", "path": vault_str }))
        .await;
    assert_eq!(status, 200, "收编应成功：{body}");
    assert_eq!(body["rootPath"].as_str().unwrap(), vault_str);
    let space_id = body["spaceId"].as_str().unwrap().to_string();

    // 原有内容立即可见：树、读、索引；.atelyx 不出现
    let (status, tree) = ctx.get(&format!("/api/spaces/{space_id}/tree"), Some(&a_token), &[]).await;
    assert_eq!(status, 200);
    let names: Vec<&str> = tree.as_array().unwrap().iter().filter_map(|n| n["name"].as_str()).collect();
    assert!(names.contains(&"笔记") && names.contains(&"画布.atlx"), "{tree}");
    assert!(!names.iter().any(|n| n.starts_with('.')), "隐藏目录不进树：{tree}");
    let (status, file) = ctx
        .get(&format!("/api/spaces/{space_id}/file"), Some(&a_token), &[("path", "笔记/旧笔记.md")])
        .await;
    assert_eq!(status, 200);
    assert_eq!(file["content"], "# 旧笔记\n#收编内容\n");
    let (status, tags) = ctx.get(&format!("/api/spaces/{space_id}/tags"), Some(&a_token), &[]).await;
    assert_eq!(status, 200);
    assert!(tags.as_array().unwrap().iter().any(|t| t["tag"] == "收编内容"), "{tags}");

    // 写入真实落到原目录（真源 = 收编位置，不是数据目录）
    let (status, _) = ctx
        .put(&format!("/api/spaces/{space_id}/file"), Some(&a_token), json!({ "path": "新文件.md", "content": "写入收编目录" }))
        .await;
    assert_eq!(status, 200);
    assert_eq!(std::fs::read_to_string(vault_root.join("新文件.md")).unwrap(), "写入收编目录");
    assert!(!dir.path().join("spaces").join(&space_id).exists(), "收编空间不得在数据目录落内容");

    // 校验：相对路径 / 不存在目录 / 数据目录内 → 400；已被占用的目录 → 409
    let (status, _) = ctx.post("/api/spaces", Some(&a_token), json!({ "name": "x", "path": "relative/path" })).await;
    assert_eq!(status, 400);
    let (status, _) = ctx.post("/api/spaces", Some(&a_token), json!({ "name": "x", "path": "/no/such/dir" })).await;
    assert_eq!(status, 400);
    let (status, _) = ctx
        .post("/api/spaces", Some(&a_token), json!({ "name": "x", "path": dir.path().join("spaces").to_string_lossy() }))
        .await;
    assert_eq!(status, 400, "数据目录内不可收编");
    let (status, _) = ctx.post("/api/spaces", Some(&a_token), json!({ "name": "x", "path": vault_str })).await;
    assert_eq!(status, 409, "同一目录不可被两个空间使用");
    let (status, _) = ctx
        .post("/api/spaces", Some(&a_token), json!({ "name": "x", "path": vault_root.join("笔记").to_string_lossy() }))
        .await;
    assert_eq!(status, 400, "与其他空间内容根互相嵌套应被拒");
}

// ===== 空间配置落点（键值元数据） =====

/// 在共享空间内签发 viewer 邀请并让第四人接受，返回其令牌。A 为 owner，B 为 editor。
async fn add_viewer(ctx: &Ctx, space_id: &str) -> (String, String, String) {
    let a = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "alice", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };
    let b = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "bob", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };
    let (_, invite) = ctx
        .post(&format!("/api/spaces/{space_id}/invites"), Some(&a), json!({ "role": "viewer" }))
        .await;
    let code = invite["code"].as_str().unwrap().to_string();
    let d_token = register(ctx, "dave").await;
    let (status, accepted) = ctx.post("/api/invites/accept", Some(&d_token), json!({ "code": code })).await;
    assert_eq!(status, 200);
    assert_eq!(accepted["role"], "viewer");
    (a, b, d_token)
}

#[tokio::test]
async fn space_meta_write_read_merge_and_delete() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let (a_token, b_token, _) = add_viewer(&ctx, &space_id).await;

    // 写多个键 → 读回一致
    let (status, w) = ctx
        .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta"), Some(&a_token), Some(json!({ "values": { "theme": "dark", "sync": "on" } })), &[])
        .await;
    assert_eq!(status, 200, "owner 写空间元信息应成功：{w}");
    let (status, r) = ctx.get(&format!("/api/spaces/{space_id}/meta"), Some(&b_token), &[]).await;
    assert_eq!(status, 200);
    assert_eq!(r["values"]["theme"], "dark");
    assert_eq!(r["values"]["sync"], "on");

    // PATCH 合并：只更新 sync，theme 保留
    let (status, _) = ctx
        .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta"), Some(&a_token), Some(json!({ "values": { "sync": "off" } })), &[])
        .await;
    assert_eq!(status, 200);
    let (_, r) = ctx.get(&format!("/api/spaces/{space_id}/meta"), Some(&b_token), &[]).await;
    assert_eq!(r["values"]["theme"], "dark", "未提及键应保留");
    assert_eq!(r["values"]["sync"], "off", "提及键应被覆盖");

    // 删单键 → 读不到
    let (status, _) = ctx
        .send(reqwest::Method::DELETE, &format!("/api/spaces/{space_id}/meta"), Some(&a_token), None, &[("key", "theme")])
        .await;
    assert_eq!(status, 200);
    let (_, r) = ctx.get(&format!("/api/spaces/{space_id}/meta"), Some(&b_token), &[]).await;
    assert!(r["values"].get("theme").is_none(), "删除后不应存在：{r}");
    assert_eq!(r["values"]["sync"], "off");
    // 删不存在的键 → 404
    let (status, _) = ctx
        .send(reqwest::Method::DELETE, &format!("/api/spaces/{space_id}/meta"), Some(&a_token), None, &[("key", "nope")])
        .await;
    assert_eq!(status, 404);
}

#[tokio::test]
async fn meta_permission_and_user_scope_isolation() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let (a_token, b_token, d_token) = add_viewer(&ctx, &space_id).await;
    let c_token = register(&ctx, "carol").await; // 非成员

    // 非成员全部 meta 操作被拒
    for m in [reqwest::Method::GET, reqwest::Method::PATCH, reqwest::Method::DELETE] {
        let (status, _) = match m {
            reqwest::Method::GET => ctx.get(&format!("/api/spaces/{space_id}/meta"), Some(&c_token), &[]).await,
            reqwest::Method::PATCH => ctx
                .send(m.clone(), &format!("/api/spaces/{space_id}/meta"), Some(&c_token), Some(json!({ "values": { "x": "1" } })), &[])
                .await,
            _ => ctx
                .send(m.clone(), &format!("/api/spaces/{space_id}/meta"), Some(&c_token), None, &[("key", "x")])
                .await,
        };
        assert_eq!(status, 403, "非成员应被拒：{m}");
    }

    // viewer：读可以，写/删被拒
    let (status, _) = ctx.get(&format!("/api/spaces/{space_id}/meta"), Some(&d_token), &[]).await;
    assert_eq!(status, 200, "viewer 可读");
    let (status, _) = ctx
        .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta"), Some(&d_token), Some(json!({ "values": { "x": "1" } })), &[])
        .await;
    assert_eq!(status, 403, "viewer 不可写空间元信息");
    let (status, _) = ctx
        .send(reqwest::Method::DELETE, &format!("/api/spaces/{space_id}/meta"), Some(&d_token), None, &[("key", "x")])
        .await;
    assert_eq!(status, 403, "viewer 不可删空间元信息");

    // meta/me：两人同名键互不可见、互不可写（目录按成员 id 隔离）
    let (status, _) = ctx
        .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta/me"), Some(&a_token), Some(json!({ "values": { "theme": "alice-dark" } })), &[])
        .await;
    assert_eq!(status, 200);
    let (status, _) = ctx
        .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta/me"), Some(&b_token), Some(json!({ "values": { "theme": "bob-light" } })), &[])
        .await;
    assert_eq!(status, 200);
    let (_, ra) = ctx.get(&format!("/api/spaces/{space_id}/meta/me"), Some(&a_token), &[]).await;
    assert_eq!(ra["values"]["theme"], "alice-dark", "只应见本人键");
    let (_, rb) = ctx.get(&format!("/api/spaces/{space_id}/meta/me"), Some(&b_token), &[]).await;
    assert_eq!(rb["values"]["theme"], "bob-light");
    // B 用自己令牌读不到 A 的键内容（目录隔离，天然互不可写他人命名空间）
    let (_, rb2) = ctx.get(&format!("/api/spaces/{space_id}/meta/me"), Some(&b_token), &[]).await;
    assert_ne!(rb2["values"]["theme"], "alice-dark");
}

#[tokio::test]
async fn meta_key_validation_rejects_traversal_and_illegal() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a_token = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "alice", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };
    for bad in ["../escape", "a/../../b", "/abs", "C:\\x", ".hidden", "a/..", "a//b", "sp ace", "a/.. ", "a./b", "tailing.", "a/con/b", "COM1", "lpt3/x"] {
        let (status, _) = ctx
            .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta"), Some(&a_token), Some(json!({ "values": { bad: "v" } })), &[])
            .await;
        assert_eq!(status, 400, "非法 key 应被拒：{bad}");
    }
    // 合法含层级/点的键通过（段中/段尾的点允许，仅以 . 开头/结尾的段被禁）
    for ok in ["chat/abc", "config.json", "a.b/c", "notes/v1"] {
        let (status, _) = ctx
            .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta"), Some(&a_token), Some(json!({ "values": { ok: "v" } })), &[])
            .await;
        assert_eq!(status, 200, "合法 key 应通过：{ok}");
    }
    // 尾点键 roundtrip 被拒：Windows 会剥离文件名尾点，原样落盘会让回读与原 key 错位
    let (status, _) = ctx
        .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta"), Some(&a_token), Some(json!({ "values": { "tailing.": "v" } })), &[])
        .await;
    assert_eq!(status, 400, "尾点键写入应被拒");
}

#[tokio::test]
async fn meta_value_roundtrip_newline_chinese_and_large() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a_token = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "alice", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };
    // 换行 + 中文；value 原文落盘（非 JSON 包装）
    let value = "第一行\n第二行\t制表\n中文标点：，。、\n{\"json\":\"可整体当字符串存\"}";
    let (status, _) = ctx
        .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta"), Some(&a_token), Some(json!({ "values": { "config": value } })), &[])
        .await;
    assert_eq!(status, 200);
    let (_, r) = ctx.get(&format!("/api/spaces/{space_id}/meta"), Some(&a_token), &[]).await;
    assert_eq!(r["values"]["config"], value, "含换行/中文/类 JSON 的 value 应原样回读");

    // 1MB 大字符串往返一致
    let big = "x".repeat(1024 * 1024);
    let (status, _) = ctx
        .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta"), Some(&a_token), Some(json!({ "values": { "big": big } })), &[])
        .await;
    assert_eq!(status, 200, "1MB value 应在 10MB 上限内通过");
    let (_, r) = ctx.get(&format!("/api/spaces/{space_id}/meta"), Some(&a_token), &[]).await;
    assert_eq!(r["values"]["big"], "x".repeat(1024 * 1024));
}

#[tokio::test]
async fn content_and_meta_size_limits_enforced() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a_token = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "alice", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };
    let b_token = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "bob", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };

    // 内容写：覆盖单文件上限为 1KB，超限被拒且消息明确
    let _serial = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let _override = SizeLimitOverride::set(Some(1024), None);
    let big = "y".repeat(2048);
    let (status, body) = ctx
        .put(&format!("/api/spaces/{space_id}/file"), Some(&a_token), json!({ "path": "big.md", "content": big }))
        .await;
    assert_eq!(status, 400, "超限写入应被拒");
    assert!(body["error"].as_str().unwrap().contains("文件过大"), "错误消息应明确：{body}");
    // 限额内通过
    let (status, _) = ctx
        .put(&format!("/api/spaces/{space_id}/file"), Some(&b_token), json!({ "path": "small.md", "content": "ok" }))
        .await;
    assert_eq!(status, 200, "限额内写入应成功");
    drop(_override);

    // meta value：覆盖单键值上限为 1KB，超限被拒且消息明确
    let _override = SizeLimitOverride::set(None, Some(1024));
    let big = "z".repeat(2048);
    let (status, body) = ctx
        .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta"), Some(&a_token), Some(json!({ "values": { "k": big } })), &[])
        .await;
    assert_eq!(status, 400, "超限配置值应被拒");
    assert!(body["error"].as_str().unwrap().contains("配置值过大"), "错误消息应明确：{body}");
    // 限额内通过
    let (status, _) = ctx
        .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta"), Some(&a_token), Some(json!({ "values": { "k": "ok" } })), &[])
        .await;
    assert_eq!(status, 200);
    drop(_override);
}

#[tokio::test]
async fn viewer_role_read_only_semantics() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let (a_token, _, d_token) = add_viewer(&ctx, &space_id).await;

    // owner 先写一份内容供 viewer 读
    let (status, _) = ctx
        .put(&format!("/api/spaces/{space_id}/file"), Some(&a_token), json!({ "path": "笔记/方案.md", "content": "# 方案" }))
        .await;
    assert_eq!(status, 200);

    // viewer 写内容被拒（403），读内容放行（200）
    let (status, w) = ctx
        .put(&format!("/api/spaces/{space_id}/file"), Some(&d_token), json!({ "path": "笔记/越权.md", "content": "x" }))
        .await;
    assert_eq!(status, 403, "viewer 写内容应被拒：{w}");
    let (status, r) = ctx.get(&format!("/api/spaces/{space_id}/file"), Some(&d_token), &[("path", "笔记/方案.md")]).await;
    assert_eq!(status, 200, "viewer 读内容应放行");
    assert_eq!(r["content"], "# 方案");

    // viewer 写团队 meta 被拒（403），写自己 meta/me 放行（200）
    let (status, _) = ctx
        .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta"), Some(&d_token), Some(json!({ "values": { "sort": "x" } })), &[])
        .await;
    assert_eq!(status, 403, "viewer 写团队元信息应被拒");
    let (status, _) = ctx
        .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta/me"), Some(&d_token), Some(json!({ "values": { "theme": "viewer-dark" } })), &[])
        .await;
    assert_eq!(status, 200, "viewer 写自身 user 元信息应放行");

    // viewer 可读成员名册（对空间内成员可见，viewer 只读）
    let (status, members) = ctx.get(&format!("/api/spaces/{space_id}/members"), Some(&d_token), &[]).await;
    assert_eq!(status, 200, "viewer 读成员名册应放行");
    let rows = members.as_array().unwrap();
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().any(|m| m["role"] == "viewer"));
}

// ===== 内容写入中心化（补丁端点 / 定序 / 乐观锁 / 广播）=====

/// 登录既有账号取令牌（用户名/密码由建号处保证）。
async fn login_as(ctx: &Ctx, username: &str) -> String {
    let (status, body) = ctx
        .post("/api/auth/login", None, json!({ "username": username, "password": "pass-123456" }))
        .await;
    assert_eq!(status, 200);
    body["token"].as_str().unwrap().to_string()
}

fn canvas_doc(id: &str, title: &str, nodes: Value) -> Value {
    json!({
        "schema": "atelyx-canvas/v1",
        "id": id,
        "title": title,
        "nodes": nodes,
        "edges": [],
        "createdAt": 1000,
        "updatedAt": 1000
    })
}

fn text_node(id: &str, x: f64) -> Value {
    json!({ "id": id, "type": "text", "x": x, "y": 0.0, "data": { "bodyMd": format!("节点{id}") } })
}

fn table_doc(id: &str, title: &str, fields: Value, rows: Value) -> Value {
    json!({
        "schema": "atelyx-table/v1",
        "id": id,
        "title": title,
        "fields": fields,
        "rows": rows,
        "createdAt": 1000,
        "updatedAt": 1000
    })
}

/// 读空间文件并按 JSON 解析（画布/表格用例的内容断言入口）。
async fn read_json(ctx: &Ctx, token: &str, space_id: &str, path: &str) -> Value {
    let (status, body) = ctx
        .get(&format!("/api/spaces/{space_id}/file"), Some(token), &[("path", path)])
        .await;
    assert_eq!(status, 200, "读取 {path} 应成功：{body}");
    serde_json::from_str(body["content"].as_str().expect("内容须为字符串")).expect("文件须为合法 JSON")
}

#[tokio::test]
async fn canvas_patch_merge_conflict_and_rename() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a = login_as(&ctx, "alice").await;
    let (_, _, viewer) = add_viewer(&ctx, &space_id).await;

    let file = "画布/设计.atlx";
    let (status, _) = ctx
        .put(
            &format!("/api/spaces/{space_id}/file"),
            Some(&a),
            json!({ "path": file, "content": canvas_doc("cv-1", "设计", json!([text_node("n1", 1.0)])).to_string() }),
        )
        .await;
    assert_eq!(status, 200);
    let url = format!("/api/spaces/{space_id}/patches/canvas");

    // upsert 新节点：按 id 追加
    let patch = json!({ "id": "cv-1", "upsertNodes": [text_node("n2", 2.0)] });
    let (status, resp) = ctx.post(&url, Some(&a), json!({ "path": file, "patch": patch })).await;
    assert_eq!(status, 200, "补丁应成功：{resp}");
    assert_eq!(resp["file"], file, "无改名时路径不变：{resp}");
    assert!(resp["updatedAt"].is_number());
    let doc = read_json(&ctx, &a, &space_id, file).await;
    let ids: Vec<&str> = doc["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["n1", "n2"]);

    // upsert 更新既有节点：整节点替换（非字段级合并）
    let mut updated = text_node("n1", 10.0);
    updated["data"] = json!({ "bodyMd": "替换后" });
    let patch = json!({ "id": "cv-1", "upsertNodes": [updated] });
    let (status, _) = ctx.post(&url, Some(&a), json!({ "path": file, "patch": patch })).await;
    assert_eq!(status, 200);
    let doc = read_json(&ctx, &a, &space_id, file).await;
    assert_eq!(doc["nodes"].as_array().unwrap().len(), 2);
    let n1 = doc["nodes"].as_array().unwrap().iter().find(|n| n["id"] == "n1").unwrap();
    assert_eq!(n1["x"], 10.0);
    assert_eq!(n1["data"]["bodyMd"], "替换后");

    // 删除（含不存在的 id：幂等不报错）
    let patch = json!({ "id": "cv-1", "removedNodeIds": ["n2", "ghost"], "removedEdgeIds": ["ghost-edge"] });
    let (status, body) = ctx.post(&url, Some(&a), json!({ "path": file, "patch": patch })).await;
    assert_eq!(status, 200, "含不存在 id 的删除应幂等成功：{body}");
    let doc = read_json(&ctx, &a, &space_id, file).await;
    let ids: Vec<&str> = doc["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["n1"]);

    // patch.id 与文件内画布 id 不符 → 400
    let patch = json!({ "id": "other-canvas", "upsertNodes": [text_node("n9", 9.0)] });
    let (status, body) = ctx.post(&url, Some(&a), json!({ "path": file, "patch": patch })).await;
    assert_eq!(status, 400, "补丁与文件不匹配应 400：{body}");

    // baseUpdatedAt 过期 → 409，body 带当前 updatedAt；带当前基准 → 200
    let patch = json!({ "id": "cv-1", "upsertNodes": [text_node("n3", 3.0)] });
    let (status, body) = ctx.post(&url, Some(&a), json!({ "path": file, "patch": patch, "baseUpdatedAt": 0 })).await;
    assert_eq!(status, 409, "过期基准应 409：{body}");
    assert!(body["error"].is_string(), "冲突 body 应带 error：{body}");
    assert!(body["updatedAt"].as_i64().unwrap() > 0, "冲突 body 应带当前 updatedAt：{body}");
    let base = body["updatedAt"].as_i64().unwrap();
    let (status, _) = ctx.post(&url, Some(&a), json!({ "path": file, "patch": patch, "baseUpdatedAt": base })).await;
    assert_eq!(status, 200, "以冲突返回的 updatedAt 为基准应成功");

    // title 变更 → 同目录改名，返回新路径，旧文件不存在
    let patch = json!({ "id": "cv-1", "title": "新设计", "upsertNodes": [] });
    let (status, resp) = ctx.post(&url, Some(&a), json!({ "path": file, "patch": patch })).await;
    assert_eq!(status, 200, "改名补丁应成功：{resp}");
    assert_eq!(resp["file"], "画布/新设计.atlx");
    let (status, _) = ctx.get(&format!("/api/spaces/{space_id}/file"), Some(&a), &[("path", file)]).await;
    assert_eq!(status, 404, "旧路径应不存在");
    let doc = read_json(&ctx, &a, &space_id, "画布/新设计.atlx").await;
    assert_eq!(doc["title"], "新设计");

    // 目标文件损坏 → 400 明确报错，不静默覆盖
    let bad = "坏文件.atlx";
    let (status, _) = ctx
        .put(&format!("/api/spaces/{space_id}/file"), Some(&a), json!({ "path": bad, "content": "not json{{{" }))
        .await;
    assert_eq!(status, 200);
    let patch = json!({ "id": "cv-1", "upsertNodes": [] });
    let (status, body) = ctx.post(&url, Some(&a), json!({ "path": bad, "patch": patch })).await;
    assert_eq!(status, 400, "损坏文件应 400：{body}");
    assert!(body["error"].is_string());

    // viewer 发补丁 → 403
    let patch = json!({ "id": "cv-1", "upsertNodes": [] });
    let (status, _) = ctx
        .post(&url, Some(&viewer), json!({ "path": "画布/新设计.atlx", "patch": patch }))
        .await;
    assert_eq!(status, 403, "viewer 发补丁应被拒");
}

#[tokio::test]
async fn table_patch_merge_order_and_force() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a = login_as(&ctx, "alice").await;

    let file = "表格/任务.atb";
    let fields = json!([
        { "id": "f1", "name": "名称", "type": "text" },
        { "id": "f2", "name": "状态", "type": "text" }
    ]);
    let rows = json!([
        { "id": "r1", "values": { "f1": "任务一" } },
        { "id": "r2", "values": { "f1": "任务二" } }
    ]);
    let (status, _) = ctx
        .put(
            &format!("/api/spaces/{space_id}/file"),
            Some(&a),
            json!({ "path": file, "content": table_doc("tb-1", "任务", fields, rows).to_string() }),
        )
        .await;
    assert_eq!(status, 200);
    let url = format!("/api/spaces/{space_id}/patches/table");

    // upsert 字段/行 + rowOrder：未出现 id（r2）保持相对顺序置尾
    let patch = json!({
        "id": "tb-1",
        "upsertFields": [ { "id": "f3", "name": "优先级", "type": "text" } ],
        "upsertRows": [ { "id": "r3", "values": { "f1": "任务三" } } ],
        "rowOrder": ["r3", "r1"]
    });
    let (status, resp) = ctx.post(&url, Some(&a), json!({ "path": file, "patch": patch })).await;
    assert_eq!(status, 200, "补丁应成功：{resp}");
    assert_eq!(resp["file"], file);
    let doc = read_json(&ctx, &a, &space_id, file).await;
    let row_ids: Vec<&str> = doc["rows"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(row_ids, vec!["r3", "r1", "r2"], "rowOrder 未出现 id 应置尾：{doc}");
    let field_ids: Vec<&str> = doc["fields"].as_array().unwrap().iter().map(|f| f["id"].as_str().unwrap()).collect();
    assert_eq!(field_ids, vec!["f1", "f2", "f3"]);

    // fieldOrder 重排：未出现 id 置尾
    let patch = json!({ "id": "tb-1", "fieldOrder": ["f3", "f2"] });
    let (status, _) = ctx.post(&url, Some(&a), json!({ "path": file, "patch": patch })).await;
    assert_eq!(status, 200);
    let doc = read_json(&ctx, &a, &space_id, file).await;
    let field_ids: Vec<&str> = doc["fields"].as_array().unwrap().iter().map(|f| f["id"].as_str().unwrap()).collect();
    assert_eq!(field_ids, vec!["f3", "f2", "f1"]);

    // 两个未出现 id 保持相对顺序置尾：当前行序 [r3, r1, r2]，rowOrder 只含 r2 → r2, r3, r1
    let patch = json!({ "id": "tb-1", "rowOrder": ["r2"] });
    let (status, _) = ctx.post(&url, Some(&a), json!({ "path": file, "patch": patch })).await;
    assert_eq!(status, 200);
    let doc = read_json(&ctx, &a, &space_id, file).await;
    let row_ids: Vec<&str> = doc["rows"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(row_ids, vec!["r2", "r3", "r1"]);

    // 删行
    let patch = json!({ "id": "tb-1", "removedRowIds": ["r1"] });
    let (status, _) = ctx.post(&url, Some(&a), json!({ "path": file, "patch": patch })).await;
    assert_eq!(status, 200);
    let doc = read_json(&ctx, &a, &space_id, file).await;
    let row_ids: Vec<&str> = doc["rows"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert_eq!(row_ids, vec!["r2", "r3"]);

    // baseUpdatedAt 冲突 → 409；force=true 跳过冲突检查 → 200
    let patch = json!({ "id": "tb-1", "upsertRows": [ { "id": "r4", "values": { "f1": "任务四" } } ] });
    let (status, body) = ctx
        .post(&url, Some(&a), json!({ "path": file, "patch": patch, "baseUpdatedAt": 0 }))
        .await;
    assert_eq!(status, 409, "过期基准应 409：{body}");
    assert!(body["updatedAt"].as_i64().unwrap() > 0);
    let (status, resp) = ctx
        .post(&url, Some(&a), json!({ "path": file, "patch": patch, "baseUpdatedAt": 0, "force": true }))
        .await;
    assert_eq!(status, 200, "force 应跳过冲突检查：{resp}");
    let doc = read_json(&ctx, &a, &space_id, file).await;
    let row_ids: Vec<&str> = doc["rows"].as_array().unwrap().iter().map(|r| r["id"].as_str().unwrap()).collect();
    assert!(row_ids.contains(&"r4"), "force 补丁应已落地：{doc}");

    // patch.id 不匹配 → 400
    let patch = json!({ "id": "other-table", "upsertRows": [] });
    let (status, body) = ctx.post(&url, Some(&a), json!({ "path": file, "patch": patch })).await;
    assert_eq!(status, 400, "补丁与文件不匹配应 400：{body}");
}

#[tokio::test]
async fn same_path_writes_serialize_no_lost_update() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a = login_as(&ctx, "alice").await;

    let file = "并发.atlx";
    let (status, _) = ctx
        .put(
            &format!("/api/spaces/{space_id}/file"),
            Some(&a),
            json!({ "path": file, "content": canvas_doc("cv-1", "并发", json!([text_node("n1", 1.0)])).to_string() }),
        )
        .await;
    assert_eq!(status, 200);

    // 同路径并发 8 个补丁各 upsert 一个新节点：串行读改写 → 全部保留（无丢失更新）
    let url = format!("/api/spaces/{space_id}/patches/canvas");
    let patch_futs = (0..8u32).map(|i| {
        let ctx = &ctx;
        let a = &a;
        let url = &url;
        async move {
            let patch = json!({ "id": "cv-1", "upsertNodes": [text_node(&format!("p{i}"), i as f64)] });
            ctx.post(url, Some(a), json!({ "path": file, "patch": patch })).await
        }
    });
    let results = join_all(patch_futs).await;
    for (status, body) in &results {
        assert_eq!(*status, 200, "并发补丁应全部成功：{body}");
    }
    let doc = read_json(&ctx, &a, &space_id, file).await;
    let ids: Vec<&str> = doc["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), 9, "n1 + 8 个并发新增节点应全部在场（串行合并不丢更新）：{ids:?}");
    for i in 0..8u32 {
        assert!(ids.contains(&&*format!("p{i}")), "p{i} 不应丢失：{ids:?}");
    }

    // 补丁与整写混发同路径：最终文件为某一写者的完整内容（原子写无交叉），始终可解析且 id 不变
    let whole = canvas_doc("cv-1", "并发", json!([text_node("n1", 1.0), text_node("pW", 50.0)])).to_string();
    let file_url = format!("/api/spaces/{space_id}/file");
    let put_fut = ctx.put(&file_url, Some(&a), json!({ "path": file, "content": whole }));
    let patch_fut = async {
        let patch = json!({ "id": "cv-1", "upsertNodes": [text_node("pM", 60.0)] });
        ctx.post(&url, Some(&a), json!({ "path": file, "patch": patch })).await
    };
    let (put_result, patch_result) = futures_util::future::join(put_fut, patch_fut).await;
    for (status, body) in [put_result, patch_result] {
        assert_eq!(status, 200, "混发写应全部成功：{body}");
    }
    let doc = read_json(&ctx, &a, &space_id, file).await;
    assert_eq!(doc["id"], "cv-1", "文件始终完整（无交叉损坏）：{doc}");
    let ids: Vec<&str> = doc["nodes"].as_array().unwrap().iter().map(|n| n["id"].as_str().unwrap()).collect();
    let patch_won = ids.contains(&"pM");
    let whole_won = ids.contains(&"pW") && ids.len() == 2;
    assert!(patch_won || whole_won, "最终内容应为补丁结果或整写结果之一：{ids:?}");

    // 同路径并发整写：最终内容 = 某一写者的完整内容，无交叉拼接
    let put_futs = (0..6u32).map(|i| {
        let ctx = &ctx;
        let a = &a;
        let space_id = &space_id;
        async move {
            let content = format!("内容-{i}");
            (
                content.clone(),
                ctx.put(
                    &format!("/api/spaces/{space_id}/file"),
                    Some(a),
                    json!({ "path": "并发笔记.md", "content": content }),
                )
                .await,
            )
        }
    });
    for (_, (status, body)) in join_all(put_futs).await {
        assert_eq!(status, 200, "并发整写应全部成功：{body}");
    }
    let (_, body) = ctx.get(&format!("/api/spaces/{space_id}/file"), Some(&a), &[("path", "并发笔记.md")]).await;
    let final_content = body["content"].as_str().unwrap();
    assert!(
        (0..6u32).map(|i| format!("内容-{i}")).any(|c| c == final_content),
        "最终内容应为某一写者的完整内容：{final_content}"
    );
}

#[tokio::test]
async fn patch_landing_broadcasts_frame_to_room() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a = login_as(&ctx, "alice").await;

    // WS 客户端入房（同一令牌 = 模拟发起成员自己的实时频道连接：广播含发送方自身）
    let mut ws = ws_connect(
        &ctx.base,
        "/ws/space",
        json!({ "type": "hello", "spaceId": space_id, "token": a, "nickname": "爱丽丝", "color": "#ff0000", "deviceName": "A机" }),
    )
    .await;
    let ack = next_frame(&mut ws).await;
    assert_eq!(ack["type"], "hello-ack");

    let file = "广播.atlx";
    let (status, _) = ctx
        .put(
            &format!("/api/spaces/{space_id}/file"),
            Some(&a),
            json!({ "path": file, "content": canvas_doc("cv-1", "广播", json!([text_node("n1", 1.0)])).to_string() }),
        )
        .await;
    assert_eq!(status, 200);

    // HTTP 画布补丁落地 → 房间收到同形状帧 {type, file, patch}
    let patch = json!({ "id": "cv-1", "upsertNodes": [text_node("n2", 2.0)] });
    let (status, _) = ctx
        .post(&format!("/api/spaces/{space_id}/patches/canvas"), Some(&a), json!({ "path": file, "patch": patch }))
        .await;
    assert_eq!(status, 200);
    let mut frame = next_frame(&mut ws).await;
    for _ in 0..5 {
        if frame["type"] == "canvas-patch" {
            break;
        }
        frame = next_frame(&mut ws).await;
    }
    assert_eq!(frame["type"], "canvas-patch", "应收到画布补丁广播帧：{frame}");
    assert_eq!(frame["file"], file);
    assert_eq!(frame["patch"]["id"], "cv-1");
    assert_eq!(frame["patch"]["upsertNodes"].as_array().unwrap().len(), 1);
    assert_eq!(frame["patch"]["upsertNodes"][0]["id"], "n2");

    // 表格补丁同型（文件名与标题一致，无改名路径漂移）
    let tfile = "广播表.atb";
    let (status, _) = ctx
        .put(
            &format!("/api/spaces/{space_id}/file"),
            Some(&a),
            json!({ "path": tfile, "content": table_doc("tb-1", "广播表", json!([{ "id": "f1", "name": "名称", "type": "text" }]), json!([])).to_string() }),
        )
        .await;
    assert_eq!(status, 200);
    let tpatch = json!({ "id": "tb-1", "upsertRows": [ { "id": "r1", "values": { "f1": "一行" } } ] });
    let (status, _) = ctx
        .post(&format!("/api/spaces/{space_id}/patches/table"), Some(&a), json!({ "path": tfile, "patch": tpatch }))
        .await;
    assert_eq!(status, 200);
    let mut frame = next_frame(&mut ws).await;
    for _ in 0..5 {
        if frame["type"] == "table-patch" {
            break;
        }
        frame = next_frame(&mut ws).await;
    }
    assert_eq!(frame["type"], "table-patch", "应收到表格补丁广播帧：{frame}");
    assert_eq!(frame["file"], tfile);
    assert_eq!(frame["patch"]["id"], "tb-1");
}

#[tokio::test]
async fn write_file_optimistic_lock() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a = login_as(&ctx, "alice").await;
    let url = format!("/api/spaces/{space_id}/file");

    let (status, w) = ctx.put(&url, Some(&a), json!({ "path": "笔记/a.md", "content": "v1" })).await;
    assert_eq!(status, 200);
    let base = w["updatedAt"].as_i64().unwrap();

    // 过期基准 → 409，body 带当前 updatedAt
    let (status, body) = ctx.put(&url, Some(&a), json!({ "path": "笔记/a.md", "content": "v2", "baseUpdatedAt": 0 })).await;
    assert_eq!(status, 409, "过期基准应 409：{body}");
    assert!(body["error"].is_string());
    assert!(body["updatedAt"].as_i64().unwrap() >= base);

    // 当前基准 → 200；缺省 = 无条件写
    let (status, _) = ctx.put(&url, Some(&a), json!({ "path": "笔记/a.md", "content": "v2", "baseUpdatedAt": base })).await;
    assert_eq!(status, 200, "以当前 updatedAt 为基准应成功");
    let (status, _) = ctx.put(&url, Some(&a), json!({ "path": "笔记/a.md", "content": "v3" })).await;
    assert_eq!(status, 200, "缺省 baseUpdatedAt 应无条件写");
    let (_, body) = ctx.get(&url, Some(&a), &[("path", "笔记/a.md")]).await;
    assert_eq!(body["content"], "v3");
}

/// 同秒两次写必须靠版本号互相识别：mtime 整秒截断下第二次写会带着恰等于当前版本的
/// 基准漏判冲突、静默覆盖第一次写入（修复目标场景）。
#[tokio::test]
async fn same_second_writes_conflict_by_version() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a = login_as(&ctx, "alice").await;
    let url = format!("/api/spaces/{space_id}/file");

    let (status, w) = ctx.put(&url, Some(&a), json!({ "path": "笔记/b.md", "content": "v1" })).await;
    assert_eq!(status, 200);
    let v1 = w["updatedAt"].as_i64().unwrap();
    let (_, r) = ctx.get(&url, Some(&a), &[("path", "笔记/b.md")]).await;
    assert_eq!(r["updatedAt"], v1, "读返回的 updatedAt 应与写返回的版本同源闭环：{r}");

    // 写者 A 以当前版本为基准成功写入（即使与上一次写落在同一秒，版本也必须推进）
    let (status, w2) = ctx
        .put(&url, Some(&a), json!({ "path": "笔记/b.md", "content": "v2", "baseUpdatedAt": v1 }))
        .await;
    assert_eq!(status, 200, "以当前版本为基准应成功：{w2}");
    let v2 = w2["updatedAt"].as_i64().unwrap();
    assert!(v2 > v1, "版本必须严格单调递增（同秒内也不停滞）：{v1} -> {v2}");

    // 写者 B 仍持同一旧基准（= A 写入前拿到的版本）→ 409，body 带当前版本
    let (status, body) = ctx
        .put(&url, Some(&a), json!({ "path": "笔记/b.md", "content": "v2-覆盖", "baseUpdatedAt": v1 }))
        .await;
    assert_eq!(status, 409, "同秒内他人写入后旧基准应 409：{body}");
    assert_eq!(body["updatedAt"], v2, "冲突 body 应带当前版本供刷新基准：{body}");
    let (_, r) = ctx.get(&url, Some(&a), &[("path", "笔记/b.md")]).await;
    assert_eq!(r["content"], "v2", "被 409 拒绝的写不得落盘：{r}");

    // 补丁端点同判据：以旧版本为基准的补丁 409，刷新基准后成功
    let cfile = "画布/版本.atlx";
    let (status, _) = ctx
        .put(
            &url,
            Some(&a),
            json!({ "path": cfile, "content": canvas_doc("cv-v", "版本", json!([text_node("n1", 1.0)])).to_string() }),
        )
        .await;
    assert_eq!(status, 200);
    let patch_url = format!("/api/spaces/{space_id}/patches/canvas");
    let patch = json!({ "id": "cv-v", "upsertNodes": [text_node("n2", 2.0)] });
    let (status, p1) = ctx.post(&patch_url, Some(&a), json!({ "path": cfile, "patch": patch })).await;
    assert_eq!(status, 200, "首次补丁（无基准）应成功：{p1}");
    let pv1 = p1["updatedAt"].as_i64().unwrap();
    let (status, body) = ctx
        .post(&patch_url, Some(&a), json!({ "path": cfile, "patch": patch, "baseUpdatedAt": pv1 - 1 }))
        .await;
    assert_eq!(status, 409, "补丁旧基准应 409：{body}");
    assert_eq!(body["updatedAt"], pv1);
    let (status, p2) = ctx
        .post(&patch_url, Some(&a), json!({ "path": cfile, "patch": patch, "baseUpdatedAt": pv1 }))
        .await;
    assert_eq!(status, 200, "以当前版本为基准的补丁应成功：{p2}");
    assert!(p2["updatedAt"].as_i64().unwrap() > pv1);
}

/// 改名边界：Windows 保留名净化（与客户端同口径）、同名异 id 拒绝覆盖、case-only 改名豁免。
#[tokio::test]
async fn rename_edges_reserved_name_id_conflict_and_case_only() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a = login_as(&ctx, "alice").await;
    let put = |path: String, id: String, title: String| {
        let ctx = &ctx;
        let a = &a;
        let space_id = &space_id;
        async move {
            ctx.put(
                &format!("/api/spaces/{space_id}/file"),
                Some(a),
                json!({ "path": path, "content": canvas_doc(&id, &title, json!([])).to_string() }),
            )
            .await
        }
    };
    let patch_title = |path: String, id: String, title: String| {
        let ctx = &ctx;
        let a = &a;
        let url = format!("/api/spaces/{space_id}/patches/canvas");
        async move {
            ctx.post(&url, Some(a), json!({ "path": path, "patch": { "id": id, "title": title } }))
                .await
        }
    };

    // title = "con"（Windows 保留名）：落地文件名净化为 _CON.atlx，与客户端 sanitize 同口径
    //（保留名不能直接建文件，走 title 改名触发 sanitize）
    let (status, _) = put("画布/旧名.atlx".into(), "cv-con".into(), "旧名".into()).await;
    assert_eq!(status, 200);
    let (status, resp) = patch_title("画布/旧名.atlx".into(), "cv-con".into(), "CON".into()).await;
    assert_eq!(status, 200, "保留名补丁应成功：{resp}");
    assert_eq!(resp["file"], "画布/_CON.atlx", "保留名应加前缀净化：{resp}");

    // 同名异 id：乙改名撞上甲已占用的名称 → 409，不静默覆盖
    let (status, _) = put("画布/甲.atlx".into(), "cv-a".into(), "甲".into()).await;
    assert_eq!(status, 200);
    let (status, _) = put("画布/乙.atlx".into(), "cv-b".into(), "乙".into()).await;
    assert_eq!(status, 200);
    let (status, body) = patch_title("画布/乙.atlx".into(), "cv-b".into(), "甲".into()).await;
    assert_eq!(status, 409, "同名异 id 改名应 409：{body}");
    assert!(body["error"].as_str().unwrap().contains("名冲突"), "409 body 应说明名冲突：{body}");
    let doc = read_json(&ctx, &a, &space_id, "画布/甲.atlx").await;
    assert_eq!(doc["id"], "cv-a", "被撞名的既有画布不得被覆盖：{doc}");

    // case-only 改名（Case → case）：大小写不敏感文件系统上指向同一物理文件，豁免名冲突成功
    let (status, _) = put("画布/Case.atlx".into(), "cv-case".into(), "Case".into()).await;
    assert_eq!(status, 200);
    let (status, resp) = patch_title("画布/Case.atlx".into(), "cv-case".into(), "case".into()).await;
    assert_eq!(status, 200, "case-only 改名应成功（同一物理文件豁免）：{resp}");
    assert_eq!(resp["file"], "画布/case.atlx");
    let doc = read_json(&ctx, &a, &space_id, "画布/case.atlx").await;
    assert_eq!(doc["title"], "case");
    // 画布列表口径：同目录只剩一个物理文件（无同 id 双文件歧义）
    let (_, tree) = ctx.get(&format!("/api/spaces/{space_id}/tree"), Some(&a), &[]).await;
    let dir_node = tree
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["path"] == "画布")
        .expect("画布目录应在树中");
    let names: Vec<&str> = dir_node["children"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    let case_count = names.iter().filter(|n| n.eq_ignore_ascii_case("case.atlx")).count();
    assert_eq!(case_count, 1, "case-only 改名后应只有一个物理文件：{names:?}");
}

#[tokio::test]
async fn meta_single_key_read() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a_token = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "alice", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };
    let b_token = {
        let (status, body) = ctx.post("/api/auth/login", None, json!({ "username": "bob", "password": "pass-123456" })).await;
        assert_eq!(status, 200);
        body["token"].as_str().unwrap().to_string()
    };

    let (status, _) = ctx
        .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta"), Some(&a_token), Some(json!({ "values": { "theme": "dark", "sync": "on" } })), &[])
        .await;
    assert_eq!(status, 200);

    // 单键读：存在 → { value }
    let (status, r) = ctx.get(&format!("/api/spaces/{space_id}/meta"), Some(&b_token), &[("key", "theme")]).await;
    assert_eq!(status, 200);
    assert_eq!(r["value"], "dark", "单键读应返回 value 字段：{r}");
    // 不存在 → 404
    let (status, _) = ctx.get(&format!("/api/spaces/{space_id}/meta"), Some(&b_token), &[("key", "nope")]).await;
    assert_eq!(status, 404, "单键读不存在的键应 404");
    // 非法 key → 400（与写路径同口径）
    let (status, _) = ctx.get(&format!("/api/spaces/{space_id}/meta"), Some(&b_token), &[("key", "bad key")]).await;
    assert_eq!(status, 400, "单键读非法 key 应 400");

    // meta/me 同口径：写本人键后单键读回
    let (status, _) = ctx
        .send(reqwest::Method::PATCH, &format!("/api/spaces/{space_id}/meta/me"), Some(&b_token), Some(json!({ "values": { "mine": "yes" } })), &[])
        .await;
    assert_eq!(status, 200);
    let (status, r) = ctx.get(&format!("/api/spaces/{space_id}/meta/me"), Some(&b_token), &[("key", "mine")]).await;
    assert_eq!(status, 200);
    assert_eq!(r["value"], "yes");
    let (status, _) = ctx.get(&format!("/api/spaces/{space_id}/meta/me"), Some(&b_token), &[("key", "nope")]).await;
    assert_eq!(status, 404);

    // 不带 key 保持全量 {values}
    let (status, r) = ctx.get(&format!("/api/spaces/{space_id}/meta"), Some(&b_token), &[]).await;
    assert_eq!(status, 200);
    assert_eq!(r["values"]["theme"], "dark");
    assert_eq!(r["values"]["sync"], "on");
}

// ===== 二进制附件（base64 读写 / 保留目录）=====

#[tokio::test]
async fn binary_file_base64_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a = login_as(&ctx, "alice").await;
    let url = format!("/api/spaces/{space_id}/file");

    // 混合字节：0x00 / 0xFF / PNG 魔数片段 / 中文 UTF-8
    let mut bytes: Vec<u8> = vec![0x00, 0xFF, 0x89, b'P', b'N', b'G', 0x0D, 0x0A];
    bytes.extend_from_slice("中文混合字节".as_bytes());
    bytes.push(0xFF);
    bytes.push(0x00);
    let encoded = B64.encode(&bytes);

    let (status, w) = ctx
        .put(&url, Some(&a), json!({ "path": "附件/图片.png", "content": encoded, "encoding": "base64" }))
        .await;
    assert_eq!(status, 200, "base64 写入应成功：{w}");
    assert!(w["updatedAt"].is_number());

    let (status, r) = ctx
        .get(&url, Some(&a), &[("path", "附件/图片.png"), ("encoding", "base64")])
        .await;
    assert_eq!(status, 200);
    assert_eq!(r["encoding"], "base64");
    assert!(r["updatedAt"].is_number());
    let decoded = B64.decode(r["content"].as_str().expect("content 须为字符串")).unwrap();
    assert_eq!(decoded, bytes, "base64 读写应逐字节一致");

    // 磁盘真相：落盘字节与写入字节逐字节一致
    let on_disk = std::fs::read(
        dir.path().join("spaces").join(&space_id).join("附件").join("图片.png"),
    )
    .unwrap();
    assert_eq!(on_disk, bytes);

    // 缺省读保持文本行为：响应形状不变（无 encoding 字段），二进制按替换字符容错
    let (status, r) = ctx.get(&url, Some(&a), &[("path", "附件/图片.png")]).await;
    assert_eq!(status, 200);
    assert!(r.get("encoding").is_none(), "缺省读不应带 encoding 字段：{r}");
    assert!(r["updatedAt"].is_number());

    // 非法 base64 / 未知编码 → 400，不静默
    let (status, body) = ctx
        .put(&url, Some(&a), json!({ "path": "坏.png", "content": "!!!不是base64!!!", "encoding": "base64" }))
        .await;
    assert_eq!(status, 400, "非法 base64 应 400：{body}");
    let (status, body) = ctx
        .put(&url, Some(&a), json!({ "path": "x.md", "content": "t", "encoding": "hex" }))
        .await;
    assert_eq!(status, 400, "写未知编码应 400：{body}");
    let (status, _) = ctx.get(&url, Some(&a), &[("path", "附件/图片.png"), ("encoding", "hex")]).await;
    assert_eq!(status, 400, "读未知编码应 400");
}

/// base64 写与文本写共用同一把路径锁与版本表：同路径并发混写不交叉损坏、
/// 限额按解码后字节、乐观锁跨编码生效。
#[tokio::test]
async fn base64_write_shares_path_lock_limit_and_version() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a = login_as(&ctx, "alice").await;
    let url = format!("/api/spaces/{space_id}/file");
    let _serial = TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner());

    // 同路径 base64 写与文本写并发混发：最终内容 = 某一写者的完整内容（路径锁互斥，无交叉拼接）
    let file = "混发.bin";
    let text_payload = "文本写者完整内容-".repeat(40);
    let mut bin: Vec<u8> = vec![0x00, 0xFF];
    bin.extend_from_slice("二进制写者完整内容".as_bytes());
    let bin_b64 = B64.encode(&bin);
    let futs = (0..8u32).map(|i| {
        let ctx = &ctx;
        let a = &a;
        let url = &url;
        let text = format!("{}{}", text_payload, i);
        let bin_b64 = bin_b64.clone();
        async move {
            if i % 2 == 0 {
                ctx.put(url, Some(a), json!({ "path": file, "content": text })).await
            } else {
                ctx.put(url, Some(a), json!({ "path": file, "content": bin_b64, "encoding": "base64" })).await
            }
        }
    });
    for (status, body) in join_all(futs).await {
        assert_eq!(status, 200, "并发混写应全部成功：{body}");
    }
    let (_, r) = ctx.get(&url, Some(&a), &[("path", file), ("encoding", "base64")]).await;
    let final_bytes = B64.decode(r["content"].as_str().expect("content 须为字符串")).unwrap();
    let mut expected: HashSet<Vec<u8>> = (0..8u32)
        .step_by(2)
        .map(|i| format!("{}{}", text_payload, i).into_bytes())
        .collect();
    expected.insert(bin.clone());
    assert!(expected.contains(&final_bytes), "最终内容应为某一写者的完整内容（无交叉损坏）");

    // 限额按解码后字节生效：override 1KB，2048 字节二进制（base64 串约 2.7KB）被拒且消息明确
    let _override = SizeLimitOverride::set(Some(1024), None);
    let big = vec![0xABu8; 2048];
    let (status, body) = ctx
        .put(&url, Some(&a), json!({ "path": "超限.bin", "content": B64.encode(&big), "encoding": "base64" }))
        .await;
    assert_eq!(status, 400, "超限 base64 写应按解码后字节被拒：{body}");
    assert!(body["error"].as_str().unwrap().contains("文件过大"), "错误消息应明确：{body}");
    drop(_override);
    drop(_serial);

    // 乐观锁对 base64 写生效：过期基准 409（带当前 updatedAt），当前基准成功
    let (status, w) = ctx
        .put(&url, Some(&a), json!({ "path": "锁.bin", "content": B64.encode(b"v1"), "encoding": "base64" }))
        .await;
    assert_eq!(status, 200);
    let base = w["updatedAt"].as_i64().unwrap();
    let (status, body) = ctx
        .put(&url, Some(&a), json!({ "path": "锁.bin", "content": B64.encode(b"v2"), "encoding": "base64", "baseUpdatedAt": 0 }))
        .await;
    assert_eq!(status, 409, "base64 写过期基准应 409：{body}");
    assert!(body["updatedAt"].as_i64().unwrap() >= base);
    let (status, _) = ctx
        .put(&url, Some(&a), json!({ "path": "锁.bin", "content": B64.encode(b"v2"), "encoding": "base64", "baseUpdatedAt": base }))
        .await;
    assert_eq!(status, 200, "以当前版本为基准的 base64 写应成功");
    let (_, r) = ctx.get(&url, Some(&a), &[("path", "锁.bin"), ("encoding", "base64")]).await;
    assert_eq!(B64.decode(r["content"].as_str().unwrap()).unwrap(), b"v2");

    // 跨编码共享版本表：文本写的版本作为 base64 写的基准仍然生效（同路径互斥同表）
    let (status, w) = ctx.put(&url, Some(&a), json!({ "path": "锁2.bin", "content": "t1" })).await;
    assert_eq!(status, 200);
    let base = w["updatedAt"].as_i64().unwrap();
    let (status, _) = ctx
        .put(&url, Some(&a), json!({ "path": "锁2.bin", "content": B64.encode(b"b1"), "encoding": "base64", "baseUpdatedAt": base }))
        .await;
    assert_eq!(status, 200, "文本写版本应可作为 base64 写基准");
    let (status, body) = ctx
        .put(&url, Some(&a), json!({ "path": "锁2.bin", "content": "t2", "baseUpdatedAt": base }))
        .await;
    assert_eq!(status, 409, "base64 写之后文本写旧基准应 409：{body}");
}

/// 保留目录：树 / glob / grep / 标签索引不出现；文件读写 API 正常可达（含 base64 二进制）。
#[tokio::test]
async fn reserved_media_dir_hidden_but_reachable() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a = login_as(&ctx, "alice").await;
    let url = format!("/api/spaces/{space_id}/file");
    let media = collab_relay::fsops::RESERVED_MEDIA_DIR;

    // 文本写与 base64 写进保留目录照常可达
    let (status, _) = ctx
        .put(&url, Some(&a), json!({ "path": format!("{media}/index.json"), "content": "{\"v\":1}" }))
        .await;
    assert_eq!(status, 200, "保留目录文本写应可达");
    let bin: Vec<u8> = vec![0x00, 0xFF, 0x89, b'P', b'N', b'G'];
    let (status, _) = ctx
        .put(&url, Some(&a), json!({ "path": format!("{media}/图片.png"), "content": B64.encode(&bin), "encoding": "base64" }))
        .await;
    assert_eq!(status, 200, "保留目录 base64 写应可达");
    let (status, _) = ctx
        .put(&url, Some(&a), json!({ "path": format!("{media}/标签.md"), "content": "#媒体\n#内部标签\n" }))
        .await;
    assert_eq!(status, 200);

    // 读写可达：二进制逐字节一致，文本原样回读
    let (status, r) = ctx
        .get(&url, Some(&a), &[("path", &format!("{media}/图片.png")), ("encoding", "base64")])
        .await;
    assert_eq!(status, 200);
    assert_eq!(B64.decode(r["content"].as_str().unwrap()).unwrap(), bin);
    let (status, r) = ctx.get(&url, Some(&a), &[("path", &format!("{media}/index.json"))]).await;
    assert_eq!(status, 200);
    assert_eq!(r["content"], "{\"v\":1}");

    // 树不出现
    let (status, tree) = ctx.get(&format!("/api/spaces/{space_id}/tree"), Some(&a), &[]).await;
    assert_eq!(status, 200);
    let tree_str = serde_json::to_string(&tree).unwrap();
    assert!(!tree_str.contains(media), "保留目录不进树：{tree_str}");

    // glob 不出现；显式指向保留目录被拒（与隐藏目录同口径）
    let (status, g) = ctx.post(&format!("/api/spaces/{space_id}/glob"), Some(&a), json!({ "pattern": "*.png" })).await;
    assert_eq!(status, 200);
    assert_eq!(g["total"], 0, "保留目录不进 glob：{g}");
    let (status, _) = ctx
        .post(&format!("/api/spaces/{space_id}/glob"), Some(&a), json!({ "pattern": "*.png", "path": media }))
        .await;
    assert_eq!(status, 400, "检索不可显式指向保留目录");

    // grep 不出现
    let (status, grep) = ctx
        .post(&format!("/api/spaces/{space_id}/grep"), Some(&a), json!({ "pattern": "内部标签" }))
        .await;
    assert_eq!(status, 200);
    assert_eq!(grep["total"], 0, "保留目录不进 grep：{grep}");

    // 标签索引不出现
    let (status, tags) = ctx.get(&format!("/api/spaces/{space_id}/tags"), Some(&a), &[]).await;
    assert_eq!(status, 200);
    assert!(
        !tags.as_array().unwrap().iter().any(|t| t["tag"] == "内部标签"),
        "保留目录不进标签索引：{tags}"
    );
}

/// 保留媒体目录枚举端点：递归列文件（name 相对查询前缀 + 字节大小）、空目录 / 不存在
/// 返回空 entries、穿越与普通内容路径 400、viewer 可读。
#[tokio::test]
async fn media_list_enumerates_reserved_dir_safely() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);
    let space_id = setup_two_members(&ctx, "alice", "bob").await;
    let a = login_as(&ctx, "alice").await;
    let (_, _, viewer) = add_viewer(&ctx, &space_id).await;
    let file_url = format!("/api/spaces/{space_id}/file");
    let url = format!("/api/spaces/{space_id}/media/list");
    let media = collab_relay::fsops::RESERVED_MEDIA_DIR;

    // 造数据：保留目录根一个文件 + 子目录两层各一个文件（size = 内容字节数）+ 一个空目录
    for (path, content) in [
        (format!("{media}/top.bin"), "12345"),
        (format!("{media}/临时/inner.txt"), "xy"),
        (format!("{media}/临时/sub/deep.txt"), "abcdef"),
    ] {
        let (status, w) = ctx.put(&file_url, Some(&a), json!({ "path": path, "content": content })).await;
        assert_eq!(status, 200, "保留目录写应可达：{w}");
    }
    let (status, _) = ctx
        .post(&format!("/api/spaces/{space_id}/folder"), Some(&a), json!({ "path": format!("{media}/empty") }))
        .await;
    assert_eq!(status, 200);

    // 全量枚举：递归含子目录，只列文件不列目录，name 相对保留目录根
    let (status, body) = ctx.get(&url, Some(&a), &[]).await;
    assert_eq!(status, 200, "全量枚举应成功：{body}");
    let entries = body["entries"].as_array().unwrap();
    let find = |name: &str| {
        entries
            .iter()
            .find(|e| e["name"] == name)
            .cloned()
            .unwrap_or(Value::Null)
    };
    assert_eq!(find("top.bin")["size"], 5);
    assert_eq!(find("临时/inner.txt")["size"], 2);
    assert_eq!(find("临时/sub/deep.txt")["size"], 6);
    assert_eq!(entries.len(), 3, "只列文件不列目录：{entries:?}");

    // 带 path 枚举子目录：name 相对查询前缀
    let (status, body) = ctx.get(&url, Some(&a), &[("path", &format!("{media}/临时"))]).await;
    assert_eq!(status, 200, "子目录枚举应成功：{body}");
    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["inner.txt", "sub/deep.txt"]);

    // 空目录与不存在的路径 → 空 entries
    let (status, body) = ctx.get(&url, Some(&a), &[("path", &format!("{media}/empty"))]).await;
    assert_eq!(status, 200);
    assert_eq!(body["entries"].as_array().unwrap().len(), 0, "空目录应空 entries：{body}");
    let (status, body) = ctx.get(&url, Some(&a), &[("path", &format!("{media}/不存在"))]).await;
    assert_eq!(status, 200);
    assert_eq!(body["entries"].as_array().unwrap().len(), 0, "不存在的路径应空 entries：{body}");

    // 越界 path：穿越段 400
    let (status, body) = ctx.get(&url, Some(&a), &[("path", "../../notes/x")]).await;
    assert_eq!(status, 400, "穿越路径应 400：{body}");

    // 普通内容路径不可由此端点访问：存在的普通目录与文件、不存在的普通路径均 400
    let (status, _) = ctx
        .put(&file_url, Some(&a), json!({ "path": "笔记/普通.md", "content": "普通" }))
        .await;
    assert_eq!(status, 200);
    for p in ["笔记/普通.md", "笔记", "普通区"] {
        let (status, body) = ctx.get(&url, Some(&a), &[("path", p)]).await;
        assert_eq!(status, 400, "普通内容路径 {p} 应被拒：{body}");
    }
    // 保留目录名的相似前缀不误放行
    let (status, body) = ctx.get(&url, Some(&a), &[("path", &format!("{media}X/f"))]).await;
    assert_eq!(status, 400, "保留目录名相似前缀应被拒：{body}");

    // viewer 可读（与内容读一致）
    let (status, body) = ctx.get(&url, Some(&viewer), &[]).await;
    assert_eq!(status, 200, "viewer 应可枚举保留目录：{body}");
    assert_eq!(body["entries"].as_array().unwrap().len(), 3);

    // 非成员 403
    let (status, _) = ctx.get(&url, None, &[]).await;
    assert_eq!(status, 401);
}

// ===== CORS（桌面 WebView 跨源直连）=====

/// WebView（origin = tauri.localhost）跨源请求先发预检 OPTIONS：服务端必须 200 应答并放行
/// POST / content-type / authorization；实际跨源请求的响应须携带 allow-origin，
/// 否则浏览器引擎直接中断请求（Failed to fetch）。
#[tokio::test]
async fn cors_preflight_and_cross_origin_requests_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Ctx::new(spawn_server(dir.path()).await);

    // 预检：模拟 WebView origin + 实际请求形态（JSON 体 + Bearer 头）
    let pre = ctx
        .http
        .request(reqwest::Method::OPTIONS, format!("{}/api/auth/register", ctx.base))
        .header("Origin", "http://tauri.localhost")
        .header("Access-Control-Request-Method", "POST")
        .header("Access-Control-Request-Headers", "content-type, authorization")
        .send()
        .await
        .expect("预检请求失败");
    assert_eq!(pre.status(), 200, "预检必须 200 放行");
    let h = pre.headers();
    assert_eq!(h.get("access-control-allow-origin").and_then(|v| v.to_str().ok()), Some("*"));
    let methods = h.get("access-control-allow-methods").and_then(|v| v.to_str().ok()).unwrap_or("");
    assert!(methods == "*" || methods.split(',').any(|m| m.trim() == "POST"), "预检应放行 POST：{methods}");
    let allow = h
        .get("access-control-allow-headers")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    assert!(allow.contains("content-type"), "预检应放行 content-type：{allow}");
    assert!(allow.contains("authorization"), "预检应放行 authorization：{allow}");

    // 实际跨源请求：响应必须携带 allow-origin，否则浏览器引擎丢弃响应
    let res = ctx
        .http
        .post(format!("{}/api/auth/register", ctx.base))
        .header("Origin", "http://tauri.localhost")
        .json(&json!({ "username": "cors_user", "password": "pass-123456" }))
        .send()
        .await
        .expect("跨源注册请求失败");
    assert!(res.status().is_success(), "跨源注册应成功");
    assert_eq!(
        res.headers().get("access-control-allow-origin").and_then(|v| v.to_str().ok()),
        Some("*"),
        "实际响应应携带 allow-origin"
    );
}
