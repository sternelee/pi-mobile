//! preview —— D15：agent 自写 html/js/css 的预览服务。
//!
//! ## 为什么需要一个真实的 HTTP 源
//!
//! 多文件项目靠**相对路径**互相引用（html → css/js/图片）。必须要有真源：
//! * 不能内联成 `srcdoc`：会打断相对路径，还要手工处理转义。
//! * 也不能靠「Tauri command 隧道」那类 IPC 转发（如 tauri-axum-htmx 的做法）：
//!   `<link href>`. `import "./app.js"`、`<img src>` 这些是**浏览器引擎自己发起**
//!   的请求，**不经过页面 JS**，所以 JS 层拦截根本看不见它们（要装 Service Worker，
//!   而它需要 secure context，在移动端自定义 scheme 上不可靠）。
//!
//! ## 为什么是**独立端口**
//!
//! 预览跑的是 **agent（LLM）写出来的 JS**，所以「它与谁能同源」是承载性的：
//! 与 `/hostcall`（`pi_bun::loopback`）**不同源**是纵深防御；真正的防线仍是
//! `script::REQUIRE_HOST_TOKEN`（预览页拿不到 host token，自己去打也是拒，
//! 已在真机验证）。两层不重复：换端口挡「意外可达」，token 挡「故意可达」。
//!
//! ## 为什么用 axum + ServeDir 而不是手写 HTTP
//!
//! 初版是 ~120 行手写 HTTP/1.1 解析——手写 HTTP 是经典 bug 重灾区，且 Range/
//! keep-alive/HEAD 语义都得自己维护。改用 `tower_http::services::ServeDir`。
//!
//! **但换库不等于安全自动到手**（这是当时换的时候就说好的）：ServeDir 的默认
//! 行为必须自己审、自己补测试。已确认的两点：
//! * **不列目录**：ServeDir 没有目录列表能力（只支持补 `index.html`），所以不会
//!   泄露工作区文件名。
//! * **会跟随符号链接**（这是 ServeDir 与手写版**共有**的风险）→ 本模块额外加了
//!   `deny_escape` 中间件做 canonicalize + 前缀校验。没有它，工作区里一个指向
//!   外部的符号链接就能读到 `creds.json`。
//!
//! ## 有意接受的风险（D15）
//!
//! 用户选择「允许脚本 + 允许联网」：预览页可以把数据发到任意外网。本模块**不拦**
//! 网络。能外泄的只有页面自己能生成的、或先经审批写进 workspace 的东西。

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::services::ServeDir;

static PORT: OnceLock<u16> = OnceLock::new();

/// 启动（幂等）。返回预览端口。
///
/// 懒启动：只在 UI 真要开预览时 bind，不给 app 启动路径加东西。
pub fn start() -> Result<u16, String> {
    if let Some(p) = PORT.get() {
        return Ok(*p);
    }
    let root = crate::pi_bun::loopback::workspace_dir().ok_or("workspace not configured")?;

    // 用 std 同步 bind：**端口必须在返回前确定**（UI 要拿它拼 iframe src）。
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("preview bind: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("preview addr: {e}"))?
        .port();

    // ⚠️ 端口**只在服务真的就绪后**才进 PORT（修 A2）。
    //
    // 初版是 bind 完立刻 `PORT.set(port)`，而 `set_nonblocking`/`from_std`/
    // `axum::serve` 都在 spawn 出去的异步任务里 —— 任一步失败就会出现「端口已
    // 缓存、服务并不在听」：`start()` 永远返回 Ok，UI 拼出一个指向死服务的
    // iframe，而且**永不重试**。那是一个全静默的失败面。
    //
    // 于是用一条就绪信道：只有 listener 注册进 reactor（from_std 成功）后才回
    // 报 Ok，失败把原因带回同步侧。`from_std` 必须在运行时上下文里做，所以
    // 这一步不能提到 `start()` 外面（hostcall 那条路径跑在普通 std 线程上，
    // 在那里调会 panic：there is no reactor running）。
    let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
    tauri::async_runtime::spawn(async move {
        if let Err(e) = listener.set_nonblocking(true) {
            let _ = tx.send(Err(format!("preview set_nonblocking: {e}")));
            return;
        }
        let l = match tokio::net::TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => {
                let _ = tx.send(Err(format!("preview from_std: {e}")));
                return;
            }
        };
        // root 的 canonical 形式只算一次：中间件每次请求都要拿它做前缀校验。
        let canon = tokio::fs::canonicalize(&root)
            .await
            .unwrap_or_else(|_| PathBuf::from(&root));
        let _ = tx.send(Ok(())); // 就绪
        if let Err(e) = axum::serve(l, router(PathBuf::from(root), canon)).await {
            log(&format!("preview serve ended: {e}"));
        }
    });

    match rx.recv_timeout(std::time::Duration::from_secs(3)) {
        Ok(Ok(())) => {
            PORT.set(port).ok();
            log(&format!("preview up: port={port}"));
            Ok(port)
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err("preview: server did not become ready in 3s".into()),
    }
}

pub fn port() -> Option<u16> {
    PORT.get().copied()
}

fn log(msg: &str) {
    crate::pi_bun::logcat(&format!("[preview] {msg}"));
}

/// 路由。**只挂 GET/HEAD** —— 预览页永不该有能力写任何东西（其他方法由 axum
/// 自动回 405）。
fn router(root: PathBuf, canon: PathBuf) -> Router {
    let state = Arc::new((root.clone(), canon));
    Router::new()
        .fallback_service(
            ServeDir::new(&root)
                // 目录请求补 index.html（相对引用才不会 404）。显式写出来，不依赖默认值。
                .append_index_html_on_directories(true),
        )
        .layer(middleware::from_fn_with_state(state, deny_escape))
        // CORS（修 A1）：iframe 是 `sandbox` 且**不给 allow-same-origin**，所以
        // 预览页是 **opaque origin** —— 它向自己那个源发 `fetch()` 会被判为跨源
        // （`Origin: null`），服务端不发 CORS 头就**会被浏览器挡掉**。
        // `<script src>`/`<link>`/`<img>` 不受影响，但 fetch/XHR 会失败，而模型
        // 写的游戏常把数据放 JSON 里 fetch 进来。
        //
        // 只放行 `Origin: null`（而不是 `*`）：普通网页发的是真实 origin，于是
        // 读不到这个工作区；沙箱预览恰好发的就是 null。读只读静态服务在最小授权
        // 下已经够用，没必要顺手把 `*` 开出去。
        .layer(
            CorsLayer::new()
                .allow_origin(AllowOrigin::exact(HeaderValue::from_static("null")))
                .allow_methods([Method::GET, Method::HEAD]),
        )
}

/// 逃逸防护：拒绝 `..`，并把**解析过符号链接之后**的路径限制在 root 内。
///
/// 为什么不能只查字符串（初版就是这样）：`workspace/link -> ../../creds.json`
/// 这种符号链接能绕过纯字符串判定，而 ServeDir 会老老实实跟随它。
type PreviewState = Arc<(PathBuf, PathBuf)>;

async fn deny_escape(
    State(state): State<PreviewState>,
    req: Request,
    next: Next,
) -> Response {
    let (_, canon) = &*state;
    let decoded = percent_decode(req.uri().path());
    let rel = decoded.trim_start_matches('/');
    if rel.split('/').any(|s| s == "..") {
        log(&format!("deny (..): {rel}"));
        return (StatusCode::FORBIDDEN, "outside workspace").into_response();
    }
    if rel.is_empty() {
        return next.run(req).await;
    }
    // canonicalize 不存在时失败 → 目标不存在，交给 ServeDir 出 404 即可
    // （符号链接指向不存在的外部路径也走这条，不会泄露）。
    if let Ok(p) = tokio::fs::canonicalize(canon.join(rel)).await {
        if !p.starts_with(canon) {
            log(&format!("deny (symlink escape): {rel}"));
            return (StatusCode::FORBIDDEN, "outside workspace").into_response();
        }
    }
    next.run(req).await
}

/// 校验一个 workspace 相对路径是否是**可预览的目标**（D15）。
///
/// 为什么必须校验「存在」：不校验的话，agent 调错路径时 UI 会打开一个空白面板，
/// 而模型从返回值看到 ok 就以为成功了 —— 接下来它会在这个错误前提上继续调试。
/// 宁可当场把「工作区里到底有哪些 html」告诉它（错误信息可执行，同 CONTRACTS
/// §2.2 的纪律）。
pub fn resolve_target(path: &str) -> Result<String, String> {
    let root = crate::pi_bun::loopback::workspace_dir().ok_or("workspace not configured")?;
    resolve_in(Path::new(&root), path)
}

/// 纯函数版（不碰全局）—— 测试能用临时目录直接验，避免与 `configure` 的
/// OnceLock 互相干扰（这个坑在本模块已经踩过一次）。
pub fn resolve_in(root: &Path, path: &str) -> Result<String, String> {
    let rel = path.trim().trim_start_matches('/');
    if rel.is_empty() {
        return Err("preview: empty path".into());
    }
    let full = crate::pi_bun::loopback::jail_path_in(root, rel)
        .map_err(|_| format!("preview: '{rel}' is outside the workspace"))?;
    if !full.is_file() {
        let mut have: Vec<String> = Vec::new();
        collect_html(root, "", 0, &mut have);
        have.sort();
        return Err(format!(
            "preview: '{rel}' does not exist in the workspace. Write it first with the write tool. \
             HTML files currently present: [{}]",
            if have.is_empty() { "none".to_string() } else { have.join(", ") }
        ));
    }
    Ok(rel.to_string())
}

/// workspace 里的 html 入口候选（给 UI 的选择列表）。
///
/// 有界：限深度与条数，并跳过 node_modules 等。**不做全量遍历**——workspace 里
/// 可能有 node_modules 之类（本仓库在无界遍历上吃过亏）。
pub fn targets() -> serde_json::Value {
    let root = match crate::pi_bun::loopback::workspace_dir() {
        Some(r) => r,
        None => return serde_json::json!([]),
    };
    let mut out: Vec<String> = Vec::new();
    collect_html(Path::new(&root), "", 0, &mut out);
    out.sort();
    serde_json::json!(out)
}

const MAX_DEPTH: usize = 4;
const MAX_TARGETS: usize = 50;
const SKIP_DIRS: &[&str] = &["node_modules", ".git", "target", "dist", ".venv"];

fn collect_html(dir: &Path, prefix: &str, depth: usize, out: &mut Vec<String>) {
    if depth > MAX_DEPTH || out.len() >= MAX_TARGETS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        if out.len() >= MAX_TARGETS {
            return;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        let rel = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        match e.file_type() {
            Ok(t) if t.is_dir() => collect_html(&e.path(), &rel, depth + 1, out),
            Ok(t) if t.is_file() => {
                let lower = name.to_ascii_lowercase();
                if lower.ends_with(".html") || lower.ends_with(".htm") {
                    out.push(rel);
                }
            }
            _ => {}
        }
    }
}

/// 最小 %XX 解码。只做这一件事，不引入 URL 依赖。
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("");
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use tower::ServiceExt; // oneshot：不起 socket 也能测真实路由栈

    fn fixture(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("pi-preview-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("app/index.html"), b"<h1>hi</h1>").unwrap();
        std::fs::write(root.join("app/site.css"), b"h1{color:red}").unwrap();
        root
    }

    fn app_for(root: &Path) -> Router {
        let canon = std::fs::canonicalize(root).unwrap();
        router(root.to_path_buf(), canon)
    }

    async fn get(app: Router, target: &str) -> (StatusCode, String, String) {
        let res = app
            .oneshot(
                Request::builder()
                    .uri(target)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let mime = res
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let body = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, mime, String::from_utf8_lossy(&body).into_owned())
    }

    #[tokio::test]
    async fn serves_html_and_css_with_right_content_types() {
        let root = fixture("mime");
        let (st, mime, body) = get(app_for(&root), "/app/index.html").await;
        assert_eq!(st, StatusCode::OK);
        // Content-Type 错了浏览器就不渲染而是下载 —— 这是最容易被换库换坏的一处
        assert!(mime.starts_with("text/html"), "{mime}");
        assert!(body.contains("<h1>hi</h1>"), "{body}");

        let (st, mime, _) = get(app_for(&root), "/app/site.css").await;
        assert_eq!(st, StatusCode::OK);
        assert!(mime.starts_with("text/css"), "{mime}");

        // 目录请求补 index.html（否则相对引用会 404）
        let (st, _, _) = get(app_for(&root), "/app/").await;
        assert_eq!(st, StatusCode::OK);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn refuses_escape_readonly_and_symlink() {
        let root = fixture("deny");
        // 逃逸诱饵：放在 root 之外
        let bait = root.parent().unwrap().join(format!("pi-bait-{}.json", std::process::id()));
        std::fs::write(&bait, b"SECRET").unwrap();

        // `..` 直接拒，且**不泄露内容**
        let (st, _, body) = get(app_for(&root), "/../pi-bait.json").await;
        assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
        assert!(!body.contains("SECRET"), "逃逸泄了内容: {body}");

        // 只读：POST 必须 405
        let res = app_for(&root)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/app/index.html")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::METHOD_NOT_ALLOWED);

        // 符号链接逃逸：这是**换 ServeDir 之后新增**的一条，因为 ServeDir 会
        // 跟随符号链接 —— 只查字符串挡不住。
        #[cfg(unix)]
        {
            let link = root.join("escape.json");
            if std::os::unix::fs::symlink(&bait, &link).is_ok() {
                let (st, _, body) = get(app_for(&root), "/escape.json").await;
                assert_eq!(st, StatusCode::FORBIDDEN, "符号链接逃逸没挡住: {body}");
                assert!(!body.contains("SECRET"), "符号链接泄了内容: {body}");
            }
            // 工作区内指向工作区内的符号链接应放行（不要过度拦截）
            let ok_link = root.join("app/alias.html");
            if std::os::unix::fs::symlink(root.join("app/index.html"), &ok_link).is_ok() {
                let (st, _, _) = get(app_for(&root), "/app/alias.html").await;
                assert_eq!(st, StatusCode::OK);
            }
        }

        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_file(&bait);
    }

    /// A1：沙箱预览页（opaque origin）发的是 `Origin: null`，必须收到
    /// `Access-Control-Allow-Origin`，否则它的 fetch/XHR 会被浏览器挡掉
    /// 而页面自己不会有任何可见报错（模型只会看到“点了没反应”）。
    #[tokio::test]
    async fn allows_opaque_origin_but_not_the_whole_web() {
        let root = fixture("cors");
        let res = app_for(&root)
            .oneshot(
                Request::builder()
                    .uri("/app/index.html")
                    .header("origin", "null")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let acao = res
            .headers()
            .get("access-control-allow-origin")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        assert_eq!(acao, "null", "沙箱预览页拿不到 ACAO 就会 fetch 失败");

        // 普通网页（真实 origin）**不该**被放行 —— 否则任意网页都能读这个工作区
        let res = app_for(&root)
            .oneshot(
                Request::builder()
                    .uri("/app/index.html")
                    .header("origin", "https://example.com")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let acao = res
            .headers()
            .get("access-control-allow-origin")
            .map(|v| v.to_str().unwrap_or("").to_string())
            .unwrap_or_default();
        assert!(
            acao.is_empty() || acao == "null",
            "不该给真实 origin 发 ACAO: {acao}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A2：`start()` 不能在服务就绪前把端口缓存下来 —— 否则一次失败会被永久
    /// 当成成功（UI 拼出指向死服务的 iframe 且永不重试）。
    ///
    /// 这里验的是可直接观测的那一半：**workspace 未配置时不能留下缓存端口**，
    /// 也就是失败路径不会污染 PORT。强制 `from_std` 失败很难构造，故那半边靠
    /// 代码结构与注释保证（就绪信号只在注册进 reactor 之后才发）。
    #[test]
    fn failed_start_does_not_poison_the_port_cache() {
        // 另一个测试可能已 configure 过（OnceLock 先到先得），所以两种结果都要
        // 能接受，只断言「失败时没有缓存」这一条性质。
        match start() {
            Ok(p) => assert!(p > 0, "成功时端口必须有效"),
            Err(_) => assert!(port().is_none(), "失败时不该留下缓存端口"),
        }
    }

    #[test]
    fn percent_decode_handles_spaces_and_utf8() {
        assert_eq!(percent_decode("/a%20b/c.html"), "/a b/c.html");
        assert_eq!(percent_decode("/%E4%B8%AD.html"), "/中.html");
        // 不完整/非法的 % 序列按字面保留，不能 panic
        assert_eq!(percent_decode("/a%zz"), "/a%zz");
        assert_eq!(percent_decode("/a%2"), "/a%2");
    }

    /// 负向优先：填错路径时必须**当场说清工作区里有什么**，否则模型会反复重试
    /// （同 CONTRACTS §2.2 的可执行指引纪律）。
    #[test]
    fn resolve_rejects_missing_escape_and_lists_what_exists() {
        let root = fixture("resolve");

        // 存在 → 通过，且归一化掉前导斜杠
        assert_eq!(resolve_in(&root, "/app/index.html").unwrap(), "app/index.html");

        // 不存在 → 报错里必须带上现有的 html，且提示先 write
        let e = resolve_in(&root, "gomoku/index.html").unwrap_err();
        assert!(e.contains("does not exist"), "{e}");
        assert!(e.contains("write tool"), "{e}");
        assert!(e.contains("app/index.html"), "应列出已有 html: {e}");

        // 逃逸 → 拒（且不能因为「列出了文件」而泄露 root 之外的东西）
        assert!(resolve_in(&root, "../secrets.json").is_err());
        assert!(resolve_in(&root, "").is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn targets_is_bounded_and_skips_noise() {
        let v = targets();
        assert!(v.is_array());
        assert!(v.as_array().unwrap().len() <= MAX_TARGETS);
    }
}
