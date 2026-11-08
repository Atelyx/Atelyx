//! 空间后端工具全链路 e2e：本地起假 HTTP 服务端（与 space_client 同一套端点契约），
//! 走 `RepoAccess::Space` 执行工具，并验证路径守卫（`..` 逃逸 / 隐藏段）在两侧一致。

use atelyx_core::ai::tools::{RepoAccess, ToolCtx};
use atelyx_core::space_client::SpaceClient;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

/// 假服务端状态：文件表（路径 → 文本）+ 用户元数据表。
#[derive(Default)]
struct FakeSpace {
    files: Mutex<HashMap<String, String>>,
    meta: Mutex<HashMap<String, String>>,
}

/// 逐请求回 `Connection: close` 的回环服务端（reqwest 每请求新建连接，串行处理即足够）。
fn spawn_server(state: Arc<FakeSpace>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("回环端口可绑");
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            if handle(stream, &state).is_err() {
                break;
            }
        }
    });
    format!("http://{addr}")
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() + 1 && i + 2 < bytes.len() {
            if let Ok(v) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    body: Value,
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Request> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let mut content_length = 0usize;
    loop {
        let mut head = String::new();
        if reader.read_line(&mut head)? == 0 {
            break;
        }
        let head = head.trim_end();
        if head.is_empty() {
            break;
        }
        if let Some(v) = head.strip_prefix("Content-Length:").or(head.strip_prefix("content-length:")) {
            content_length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body)?;
    }
    let (path, query_raw) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };
    let mut query = HashMap::new();
    for pair in query_raw.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        query.insert(percent_decode(k), percent_decode(v));
    }
    Ok(Request {
        method,
        path,
        query,
        body: serde_json::from_slice(&body).unwrap_or(Value::Null),
    })
}

fn respond(stream: &mut TcpStream, status: u16, body: Value) -> std::io::Result<()> {
    let body = serde_json::to_string(&body).unwrap_or_default();
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Bad Request",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn ok(stream: &mut TcpStream, v: Value) -> std::io::Result<()> {
    respond(stream, 200, v)
}

fn not_found(stream: &mut TcpStream) -> std::io::Result<()> {
    respond(stream, 404, json!({ "error": "not found" }))
}

fn handle(mut stream: TcpStream, state: &FakeSpace) -> std::io::Result<()> {
    let req = read_request(&mut stream)?;
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", p) if p.ends_with("/file") => {
            let path = req.query.get("path").cloned().unwrap_or_default();
            match state.files.lock().unwrap().get(&path) {
                Some(content) => ok(&mut stream, json!({ "content": content }))?,
                None => not_found(&mut stream)?,
            }
        }
        ("PUT", p) if p.ends_with("/file") => {
            let path = req.body.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
            let content = req.body.get("content").and_then(Value::as_str).unwrap_or_default().to_string();
            state.files.lock().unwrap().insert(path, content);
            ok(&mut stream, json!({ "updatedAt": 1 }))?;
        }
        ("DELETE", p) if p.ends_with("/file") => {
            let path = req.query.get("path").cloned().unwrap_or_default();
            state.files.lock().unwrap().remove(&path);
            ok(&mut stream, json!({}))?;
        }
        ("DELETE", p) if p.ends_with("/folder") => {
            let path = req.body.get("path").and_then(Value::as_str).unwrap_or_default().to_string();
            let mut files = state.files.lock().unwrap();
            files.retain(|k, _| !k.starts_with(&path));
            ok(&mut stream, json!({ "needsConfirm": false }))?;
        }
        ("POST", p) if p.ends_with("/rename") => {
            let old = req.body.get("oldPath").and_then(Value::as_str).unwrap_or_default().to_string();
            let new = req.body.get("newPath").and_then(Value::as_str).unwrap_or_default().to_string();
            let mut files = state.files.lock().unwrap();
            if let Some(content) = files.remove(&old) {
                files.insert(new, content);
            }
            ok(&mut stream, json!({}))?;
        }
        ("POST", p) if p.ends_with("/glob") => {
            // 简化匹配：pattern 精确路径或 `*.md` 全表过滤
            let pattern = req.body.get("pattern").and_then(Value::as_str).unwrap_or_default();
            let paths: Vec<String> = state
                .files
                .lock()
                .unwrap()
                .keys()
                .filter(|p| (pattern.ends_with(".md") && p.ends_with(".md")) || p.as_str() == pattern)
                .cloned()
                .collect();
            let total = paths.len() as u64;
            ok(&mut stream, json!({ "paths": paths, "total": total, "capped": false }))?;
        }
        ("POST", p) if p.ends_with("/grep") => {
            let pattern = req.body.get("pattern").and_then(Value::as_str).unwrap_or_default();
            let files = state.files.lock().unwrap();
            let mut matches: Vec<Value> = Vec::new();
            for (path, content) in files.iter() {
                for (i, line) in content.lines().enumerate() {
                    if line.contains(pattern) {
                        matches.push(json!({ "path": path, "lineNumber": i + 1, "line": line }));
                    }
                }
            }
            let total = matches.len() as u64;
            ok(&mut stream, json!({ "matches": matches, "total": total, "capped": false }))?;
        }
        ("GET", p) if p.ends_with("/tree") => {
            // 平铺文件表 → 两层树（顶层目录 → 文件），与工具的目录过滤口径对齐
            let files = state.files.lock().unwrap();
            let mut dirs: Vec<(String, Vec<Value>)> = Vec::new();
            let mut roots: Vec<Value> = Vec::new();
            for path in files.keys() {
                match path.split_once('/') {
                    Some((dir, _)) => {
                        if let Some(slot) = dirs.iter_mut().find(|(d, _)| d == dir) {
                            slot.1.push(json!({ "name": path.rsplit('/').next().unwrap_or(path), "path": path, "isDir": false, "updatedAt": 1, "children": [] }));
                        } else {
                            dirs.push((dir.to_string(), vec![json!({ "name": path.rsplit('/').next().unwrap_or(path), "path": path, "isDir": false, "updatedAt": 1, "children": [] })]));
                        }
                    }
                    None => roots.push(json!({ "name": path, "path": path, "isDir": false, "updatedAt": 1, "children": [] })),
                }
            }
            for (dir, children) in dirs {
                roots.push(json!({ "name": dir, "path": dir, "isDir": true, "updatedAt": 1, "children": children }));
            }
            ok(&mut stream, Value::Array(roots))?;
        }
        ("GET", p) if p.ends_with("/meta/me") => {
            let key = req.query.get("key").cloned().unwrap_or_default();
            match state.meta.lock().unwrap().get(&key) {
                Some(v) => ok(&mut stream, json!({ "value": v }))?,
                None => not_found(&mut stream)?,
            }
        }
        ("PATCH", p) if p.ends_with("/meta/me") => {
            if let Some(values) = req.body.get("values").and_then(Value::as_object) {
                let mut meta = state.meta.lock().unwrap();
                for (k, v) in values {
                    if let Some(s) = v.as_str() {
                        meta.insert(k.clone(), s.to_string());
                    }
                }
            }
            ok(&mut stream, json!({}))?;
        }
        _ => not_found(&mut stream)?,
    }
    Ok(())
}

fn space_ctx(server_url: &str) -> ToolCtx {
    let client = Arc::new(SpaceClient::new(server_url, "test-token".to_string()).expect("回环地址应放行"));
    ToolCtx {
        repo: RepoAccess::Space { client, space_id: "s1".to_string() },
        search: atelyx_core::ai::tools::SearchConfig::default(),
        exclude: Vec::new(),
        plugin_tools: Vec::new(),
        plugins: None,
    }
}

fn err_text(r: atelyx_core::ai::tools::ToolResult) -> String {
    assert!(r.is_error, "应失败，结果反而成功：{}", r.text);
    r.text
}

fn ok_text(r: atelyx_core::ai::tools::ToolResult) -> String {
    assert!(!r.is_error, "应成功，结果反而失败：{}", r.text);
    r.text
}

#[tokio::test]
async fn 写读改删检索全链路() {
    let state = Arc::new(FakeSpace::default());
    let url = spawn_server(state.clone());
    let ctx = space_ctx(&url);

    // 写 → 读（行号渲染）
    let out = atelyx_core::ai::tools::execute(
        &ctx,
        "write_file",
        json!({ "path": "notes/a.md", "content": "第一行\n第二行" }),
    )
    .await;
    assert!(ok_text(out).contains("已写入"));
    let out = atelyx_core::ai::tools::execute(&ctx, "read_file", json!({ "path": "notes/a.md" })).await;
    let text = ok_text(out);
    assert!(text.contains("1| 第一行") && text.contains("2| 第二行"), "实际：{text}");

    // 追加 → 编辑
    let out = atelyx_core::ai::tools::execute(&ctx, "append_file", json!({ "path": "notes/a.md", "content": "\n第三行" })).await;
    assert!(ok_text(out).contains("已追加"));
    let out = atelyx_core::ai::tools::execute(
        &ctx,
        "edit_file",
        json!({ "path": "notes/a.md", "find": "第二行", "replace": "改过的行" }),
    )
    .await;
    assert!(ok_text(out).contains("已编辑"));
    assert!(state.files.lock().unwrap()["notes/a.md"].contains("改过的行"));

    // 检索：glob + grep
    let out = atelyx_core::ai::tools::execute(&ctx, "glob", json!({ "pattern": "*.md" })).await;
    assert!(ok_text(out).contains("notes/a.md"));
    let out = atelyx_core::ai::tools::execute(&ctx, "grep", json!({ "pattern": "改过的行" })).await;
    let text = ok_text(out);
    assert!(text.contains("notes/a.md:2: 改过的行"), "实际：{text}");

    // 目录列举（树 → 直接子项）
    let out = atelyx_core::ai::tools::execute(&ctx, "list_dir", json!({ "dir": "notes" })).await;
    assert!(ok_text(out).contains("a.md"));

    // 历史（假服务端无侧文件 → 尽力而为空列表）
    let out = atelyx_core::ai::tools::execute(&ctx, "read_history", json!({ "path": "notes/a.md" })).await;
    assert_eq!(ok_text(out), "无历史版本");

    // 重命名 → 删除
    let out = atelyx_core::ai::tools::execute(
        &ctx,
        "rename_file",
        json!({ "path": "notes/a.md", "newName": "b.md" }),
    )
    .await;
    assert!(ok_text(out).contains("已重命名"));
    assert!(state.files.lock().unwrap().contains_key("notes/b.md"));
    let out = atelyx_core::ai::tools::execute(&ctx, "delete_file", json!({ "path": "notes/b.md" })).await;
    assert!(ok_text(out).contains("已删除"));
    assert!(!state.files.lock().unwrap().contains_key("notes/b.md"));
}

#[tokio::test]
async fn 任务清单落用户元数据() {
    let state = Arc::new(FakeSpace::default());
    let url = spawn_server(state.clone());
    let ctx = space_ctx(&url);

    let out = atelyx_core::ai::tools::execute(
        &ctx,
        "todo_write",
        json!({ "todos": [ { "id": "t1", "text": "调研", "done": false } ] }),
    )
    .await;
    assert!(ok_text(out).contains("任务清单已更新"));
    let stored = state
        .meta
        .lock()
        .unwrap()
        .get(atelyx_core::ai::tools::TODO_FILE)
        .cloned()
        .expect("todo 内容应写入 meta");
    assert!(stored.contains("调研"));
}

#[tokio::test]
async fn 路径守卫在空间后端同样生效() {
    let state = Arc::new(FakeSpace::default());
    let url = spawn_server(state.clone());
    let ctx = space_ctx(&url);

    // `..` 逃逸
    let e = err_text(atelyx_core::ai::tools::execute(&ctx, "read_file", json!({ "path": "../secrets" })).await);
    assert!(e.contains("非法路径"), "实际：{e}");
    // 隐藏段（密钥不回传模型）
    let e = err_text(
        atelyx_core::ai::tools::execute(&ctx, "read_file", json!({ "path": ".atelyx/config.json" })).await,
    );
    assert!(e.contains("隐藏"), "实际：{e}");
    // 绝对路径
    let e = err_text(
        atelyx_core::ai::tools::execute(&ctx, "write_file", json!({ "path": "C:/Windows/x", "content": "x" })).await,
    );
    assert!(e.contains("非法路径"), "实际：{e}");
    // 守卫在请求之前：服务端零命中
    assert!(state.files.lock().unwrap().is_empty());
}
