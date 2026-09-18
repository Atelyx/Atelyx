//! 服务端集成测试（验收口径）：两个账号共享一个空间；非成员请求被拒；反链 / 标签 / 搜索在空间内容上可用。
//!
//! 每个用例独立临时数据目录 + 真实端口，进程内起服务器，用真实 HTTP / WS 客户端完整打请求链路，
//! 不 mock 路由与鉴权层。

use std::path::Path;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message;

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
    let (status, _) = ctx.post(&format!("/api/spaces/{space_id}/invites"), Some(&a_token), json!({ "role": "viewer" })).await;
    assert_eq!(status, 400, "viewer 角色未启用，签发应被拒");
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

    // 旧中转端点不受影响：vaultId 房间照常工作
    let mut legacy = ws_connect(&ctx.base, "/ws", json!({ "type": "hello", "vaultId": "legacy-room", "nickname": "老客户端", "color": "#000000", "deviceName": "L" })).await;
    let ack = next_frame(&mut legacy).await;
    assert_eq!(ack["type"], "hello-ack");
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
