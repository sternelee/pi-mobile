// D14 脚本 runner 验收 harness（可复现）。
//
// 跑法：
//   cd scripts/d14-runner-harness && cargo run --release -- ../../build/skal-macos-spike/libskal.dylib
// 前置：先建 dylib（见 scripts/link-skal-macos.sh 头部注释）。
//
// 关键：判定用的是**真实**的 src-tauri/src/script.rs —— build.rs 把它复制进
// OUT_DIR（只把行首 `//!` 换成 `//`，纯文档标记，因为 include! 的展开位置不允许
// 内部文档注释），所以这里跑的 authorize/required_capability 就是产品里那一份，
// 不是复制品。结尾会打印两个文件的 sha256 供核对是否漂移。
//
// 被"借"来的：
//   * authorize / required_capability / TOKEN_FIELD —— 真实策略层
//   * dispatch 的**前置检查三行**（loopback.rs:785-792）—— 逐行照抄，只去掉
//     `crate::script::` 前缀
// 本 harness 自己实现的只有「动作执行」的 stub（真实那个在 loopback.rs，依赖
// 太重）—— 本次要验的是**授权边界**，不是文件读取本身。
//
// 验收覆盖（对应任务书的 5 条）：
//   [1]  正向：授权 fs:read → 脚本经 __pi_hostcall 拿到文件内容
//   [2]  负向：未授权 / 永不可授予 / 伪造 token → 均须被 **Rust 侧** 拒
//   [3]  隔离：脚本 VM 里 agent 全局不可达
//   [4]  超时：CPU 死循环（JSC 看门狗）与 I/O 挂起（墙钟看护）各一条
//   [5]  连跑 5 次不崩
//
// ⚠️ 必须先起 agent 运行时（skal_create_runtime）：bun.jsc.initialize 是进程
// 一次性的，没有它脚本 VM 的 init 会静默杀死进程。这也是产品的真实顺序。
mod script {
    include!(concat!(env!("OUT_DIR"), "/script_included.rs"));
}
use script::{authorize, grant, revoke, TOKEN_FIELD};

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::raw::{c_char, c_int, c_void};

// ── dlopen（避免为 libloading 多下一个依赖）───────────────────────────
extern "C" {
    fn dlopen(path: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, sym: *const c_char) -> *mut c_void;
}
const RTLD_NOW: c_int = 2;

type RunScriptFn = unsafe extern "C" fn(
    token: *const u8,
    token_len: usize,
    port: u16,
    code: *const u8,
    code_len: usize,
    wall_ms: u32,
    out_result: *mut *mut u8,
    out_result_len: *mut usize,
    out_is_error: *mut c_int,
) -> c_int;
type FreeStrFn = unsafe extern "C" fn(*mut c_char);
type CreateFn = unsafe extern "C" fn(*const u8, usize) -> i64;

/// loopback.rs:785-792 的前置检查（逐行照抄，去 crate:: 前缀）。
fn enforce(method: &str, payload: &serde_json::Value) -> Result<(), serde_json::Value> {
    if let Some(token) = payload.get(TOKEN_FIELD).and_then(|v| v.as_str()) {
        authorize(token, method, payload)?;
    }
    Ok(())
}

/// 动作 stub：只实现验收要用的两条，其余回一个可辨识的占位。
/// 注意：**必须过 enforce 之后才可能到这里**。
fn execute(method: &str, payload: &serde_json::Value) -> serde_json::Value {
    let sub = payload.get("name").and_then(|v| v.as_str()).unwrap_or("");
    match (method, sub) {
        ("tool", "read") => {
            let path = payload
                .get("args")
                .and_then(|a| a.get("path"))
                .and_then(|v| v.as_str())
                .unwrap_or("?");
            serde_json::json!({ "text": format!("FILE-CONTENT-OF({path})") })
        }
        _ => serde_json::json!({ "text": format!("EXECUTED:{method}/{sub}") }),
    }
}

fn serve(listener: TcpListener) {
    for stream in listener.incoming() {
        let Ok(mut s) = stream else { continue };
        std::thread::spawn(move || {
            let _ = handle(&mut s);
        });
    }
}

fn handle(s: &mut TcpStream) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    // 读到 header 结束
    let (head_end, content_length) = loop {
        let n = s.read(&mut tmp)?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..pos]).to_string();
            let cl = head
                .lines()
                .find_map(|l| {
                    let l = l.to_ascii_lowercase();
                    l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            break (pos + 4, cl);
        }
    };
    while buf.len() < head_end + content_length {
        let n = s.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let body = &buf[head_end..(head_end + content_length).min(buf.len())];

    let out = match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(v) => {
            let m = v.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
            let payload = v.get("payload").cloned().unwrap_or(serde_json::Value::Null);
            // ↓↓↓ 产品里的同一道边界 ↓↓↓
            match enforce(&m, &payload) {
                Err(denied) => {
                    eprintln!("   [rust] DENIED {m} -> {}", denied["error"]);
                    denied
                }
                Ok(()) => {
                    eprintln!("   [rust] ALLOWED {m}");
                    execute(&m, &payload)
                }
            }
        }
        Err(e) => serde_json::json!({ "error": format!("bad json: {e}") }),
    };
    let body = serde_json::to_string(&out).unwrap();
    write!(
        s,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )?;
    s.flush()
}

fn find_subslice(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

struct Lib {
    handle: *mut c_void,
}

impl Lib {
    fn open(p: &str) -> Self {
        let c = std::ffi::CString::new(p).unwrap();
        let handle = unsafe { dlopen(c.as_ptr(), RTLD_NOW) };
        assert!(!handle.is_null(), "dlopen({p}) failed");
        Lib { handle }
    }
    fn sym(&self, n: &str) -> *mut c_void {
        let c = std::ffi::CString::new(n).unwrap();
        let p = unsafe { dlsym(self.handle, c.as_ptr()) };
        assert!(!p.is_null(), "dlsym({n}) failed");
        p
    }
}

fn main() {
    let libpath = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "build/skal-macos-spike/libskal.dylib".into());

    // ── 起 loopback（真实判定在 enforce 里）──────────────────────────────
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || serve(listener));
    println!("== loopback port={port}");

    let lib = Lib::open(&libpath);
    let create: CreateFn = unsafe { std::mem::transmute(lib.sym("skal_create_runtime")) };
    let run_script: RunScriptFn = unsafe { std::mem::transmute(lib.sym("pibun_run_script")) };
    let free_str: FreeStrFn = unsafe { std::mem::transmute(lib.sym("skal_free_string")) };
    println!("== dlopen ok: {} (pibun_run_script found)", libpath);

    // ⚠️ 必须先起 agent 运行时：bun.jsc.initialize 是进程一次性的，由 agent 的
    // workerMain 调用。没有它，第二 VM 的 init 会静默杀死进程（首轮实测）。
    // 这也正是产品的真实顺序：脚本只在 agent 已经起来之后才可能被运行。
    let datadir = "/tmp/pi-d14-data";
    let _ = std::fs::create_dir_all(datadir);
    let h = unsafe { create(datadir.as_ptr(), datadir.len()) };
    assert!(h != 0, "skal_create_runtime failed");
    println!("== agent 运行时已起（handle={h}）—— 顺序与产品一致");
    std::thread::sleep(std::time::Duration::from_millis(600));

    let call = |label: &str, token: &str, code: &str, wall_ms: u32| -> (String, bool) {
        let (mut r, mut n, mut e): (*mut u8, usize, c_int) = (std::ptr::null_mut(), 0, 0);
        let rc = unsafe {
            run_script(
                token.as_ptr(),
                token.len(),
                port,
                code.as_ptr(),
                code.len(),
                wall_ms,
                &mut r,
                &mut n,
                &mut e,
            )
        };
        let text = unsafe { std::slice::from_raw_parts(r, n) };
        let s = String::from_utf8_lossy(text).to_string();
        unsafe { free_str(r as *mut c_char) };
        println!(
            "   [{label}] rc={rc} out_is_error={e} out_len={n} out_result=\n      {}",
            s
        );
        (s, e == 1)
    };

    // ═══ 1. 正向：needs=[fs:read]，脚本经 __pi_hostcall 读文件 ═══
    println!("\n== [1] 正向：授权 fs:read 时脚本能拿到文件内容");
    let g = grant(&["fs:read".to_string()], None);
    println!("   grant: run_id={} caps={:?} wall={}ms", g.run_id, g.capabilities, g.wall_ms);
    let (r1, _) = call(
        "granted-fs-read",
        &g.token,
        r#"const res = await __pi_hostcall("tool", { name: "read", args: { path: "notes.md" } });
console.log("script got:", res.text);
return res.text;"#,
        5000,
    );
    revoke(&g.token);
    assert!(r1.contains("FILE-CONTENT-OF(notes.md)"), "正向失败: {r1}");
    assert!(r1.contains("\"ok\":true"), "信封应为 ok:true: {r1}");

    // ═══ 2. 负向（最重要）：调用未授权能力 → 必须被 Rust 侧拒 ═══
    println!("\n== [2] 负向：只授 fs:read，脚本去调 native/contacts → 必须被 Rust 拒");
    let g2 = grant(&["fs:read".to_string()], None);
    let (r2, _) = call(
        "ungranted-native",
        &g2.token,
        r#"const res = await __pi_hostcall("native", { name: "contacts", args: {} });
return { denied: res.denied === true, error: res.error, hint: res.hint };"#,
        5000,
    );
    revoke(&g2.token);
    assert!(r2.contains("\"denied\":true"), "未授权必须被拒: {r2}");
    assert!(r2.contains("native:contacts"), "错误应点名缺哪个能力: {r2}");

    // 永不可授予的通道：即使脚本硬往里塞，也必须被拒
    println!("\n== [2b] 负向：永不可授予通道（approval_request）");
    let g2b = grant(&["fs:read".to_string(), "net".to_string()], None);
    let (r2b, _) = call(
        "never-grantable",
        &g2b.token,
        r#"const res = await __pi_hostcall("approval_request", { tool: "write", args: { path: "x", content: "y" } });
console.log("RESULT", JSON.stringify(res));
return res.error || null;"#,
        5000,
    );
    revoke(&g2b.token);
    assert!(r2b.contains("is not available to scripts"), "永不可授予通道应被拒: {r2b}");

    // 伪造 token：绝不回退成 agent 身份
    println!("\n== [2c] 负向：伪造 token → 拒绝且不回退成 agent");
    let (r2c, _) = call(
        "forged-token",
        "script_deadbeefdeadbeef",
        r#"const res = await __pi_hostcall("tool", { name: "read", args: { path: "notes.md" } });
console.log("RESULT", JSON.stringify(res));
return res.error || null;"#,
        5000,
    );
    assert!(r2c.contains("invalid or expired"), "伪造 token 应被拒: {r2c}");

    // ═══ 3. 隔离：脚本 VM 里看不见 agent 的全局 ═══
    println!("\n== [3] 隔离：脚本 VM 里 agent 全局必须不可达");
    let g3 = grant(&[], None);
    let (r3, _) = call(
        "isolation",
        &g3.token,
        r#"return {
  PI_CONFIG: typeof globalThis.__PI_CONFIG,
  pi_hostcall_is_ours: typeof globalThis.__pi_hostcall,
  pi_on_event: typeof globalThis.__pi_on_event,
  node_require: typeof globalThis.require,
  fetch_exists: typeof fetch
};"#,
        5000,
    );
    revoke(&g3.token);
    assert!(r3.contains("\"PI_CONFIG\":\"undefined\""), "__PI_CONFIG 不可达: {r3}");
    assert!(r3.contains("\"pi_on_event\":\"undefined\""), "__pi_on_event 不可达: {r3}");
    assert!(r3.contains("\"node_require\":\"undefined\""), "node require 不可达: {r3}");

    // ═══ 4. 超时：CPU 死循环（看门狗）与 I/O 挂起（墙钟看护）═══
    println!("\n== [4a] 超时：CPU 死循环 while(true){{}}  (wall=1500ms)");
    let g4 = grant(&[], Some(1500));
    let t0 = std::time::Instant::now();
    let (r4, e4) = call("cpu-deadloop", &g4.token, "while (true) {}", 1500);
    let w4 = t0.elapsed().as_millis();
    revoke(&g4.token);
    assert!(e4, "CPU 死循环应以 runner 错误收场: {r4}");
    assert!(r4.contains("execution-time-limit"), "应报 execution-time-limit: {r4}");
    println!("   -> 实测墙钟 {w4}ms（上界 1500ms）");

    println!("\n== [4b] 超时：I/O 挂起 await new Promise(()=>{{}})（不消耗 CPU！）");
    let g4b = grant(&[], Some(1500));
    let t1 = std::time::Instant::now();
    let (r4b, e4b) = call(
        "io-hang",
        &g4b.token,
        "await new Promise(() => {}); return 'never';",
        1500,
    );
    let w4b = t1.elapsed().as_millis();
    revoke(&g4b.token);
    assert!(e4b, "I/O 挂起应以 runner 错误收场: {r4b}");
    assert!(r4b.contains("wall-clock-timeout"), "应报 wall-clock-timeout: {r4b}");
    println!("   -> 实测墙钟 {w4b}ms（上界 1500ms）—— 看门狗在此不触发，靠墙钟看护");

    // ═══ 5. 连跑 5 次不崩 ═══
    println!("\n== [5] 连跑 5 次（每次新 VM）不崩");
    for i in 0..5 {
        let g5 = grant(&["fs:read".to_string()], None);
        let code = format!("const r = await __pi_hostcall(\"tool\", {{ name: \"read\", args: {{ path: \"f{i}\" }} }}); return r.text;");
        let (r5, e5) = call(&format!("run{i}"), &g5.token, &code, 5000);
        revoke(&g5.token);
        assert!(!e5, "第 {i} 次不该失败: {r5}");
        assert!(r5.contains(&format!("FILE-CONTENT-OF(f{i})")), "第 {i} 次结果不对: {r5}");
    }

    println!("\n==== 全部验收通过；进程存活（走到这里即未崩）====");

    // 漂移核对：harness 用的 script.rs 副本应与源文件语义一致（只差 //! → //）
    println!("\n== 策略层未漂移核对（sha256）");
    for f in [
        "../../src-tauri/src/script.rs",
        concat!(env!("OUT_DIR"), "/script_included.rs"),
    ] {
        let out = std::process::Command::new("shasum").args(["-a", "256", f]).output();
        match out {
            Ok(o) => println!("   {}", String::from_utf8_lossy(&o.stdout).trim()),
            Err(_) => println!("   (shasum 不可用: {f})"),
        }
    }
    println!("   两者差异应仅为 //! → //（build.rs 唯一做的变换）");

    drop(lib);
}
