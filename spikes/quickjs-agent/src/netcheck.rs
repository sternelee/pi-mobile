//! net-check —— 上真机时的自诊断：把「网络能不能出去」和「引擎能不能跑」分开。
//!
//! 为什么需要它：真机上失败时最先要回答的是**是哪一层坏了**。spike 的链路里
//! 至少有三层可能各自独立地挂掉：
//!   1. DNS —— Android 上最容易出问题的一环（pi-mobile 在 iOS 上被 bun 的
//!      c-ares 坑过：读不到 /etc/resolv.conf → 去连 127.0.0.1:53 → 全挂）。
//!      这里走 rustls + `std::net` 的 getaddrinfo，理论上没问题，但**理论要验**。
//!   2. TLS —— 根证书是编译进来的（webpki-roots），所以不该依赖系统信任库；
//!      这条能过就说明「不碰 Android cacerts」的结论在真机成立。
//!   3. 引擎 —— QuickJS 能不能起来、364KB bundle 能不能 eval、pi-agent-core
//!      能不能 boot。这层完全不碰网络，所以它单独可验。
//!
//! 输出是逐行的 `ok/fail + 耗时`，设备上 `adb shell` 一眼能看。任一失败即非零退出。

use std::time::{Duration, Instant};

use crate::deepseek::DeepSeekConfig;

struct Step {
    name: &'static str,
    detail: String,
    ok: bool,
    took: Duration,
}

fn run_steps(cfg: &DeepSeekConfig) -> Vec<Step> {
    let mut steps = Vec::new();
    let host = cfg
        .base_url
        .split("://")
        .nth(1)
        .unwrap_or(&cfg.base_url)
        .split('/')
        .next()
        .unwrap_or("")
        .to_string();
    let port = if cfg.base_url.starts_with("https") {
        443
    } else {
        80
    };

    // ── 1. DNS ────────────────────────────────────────────────────────
    let started = Instant::now();
    let resolved = std::net::ToSocketAddrs::to_socket_addrs(&(host.as_str(), port));
    let (ok, detail) = match resolved {
        Ok(addrs) => {
            let list: Vec<String> = addrs
                .into_iter()
                .take(3)
                .map(|a| a.ip().to_string())
                .collect();
            (true, format!("{host} → {}", list.join(", ")))
        }
        Err(error) => (false, format!("{host} 解析失败: {error}")),
    };
    steps.push(Step {
        name: "dns",
        detail,
        ok,
        took: started.elapsed(),
    });
    if !ok {
        return steps; // 后面都依赖它，不必再跑
    }

    // ── 2. TCP + TLS（走 reqwest/rustls，与正式请求同一条路）─────────
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            steps.push(Step {
                name: "tls",
                detail: format!("client 构建失败: {error}"),
                ok: false,
                took: Duration::ZERO,
            });
            return steps;
        }
    };
    let started = Instant::now();
    // 拿一个不需要鉴权的路径：401/404 都说明「TCP+TLS 通了、HTTP 有应答」。
    // 用 GET /models 更贴近 Provider 真实端点，但 401 也算过。
    let probe = client
        .get(format!("{}/models", cfg.base_url.trim_end_matches('/')))
        .send();
    let (ok, detail) = match probe {
        Ok(response) => (
            true,
            format!(
                "{} (HTTP {}) — 证书由编译进来的 webpki 根校验，未用系统信任库",
                host,
                response.status().as_u16()
            ),
        ),
        Err(error) => {
            let mut text = format!("{error}");
            // reqwest 的 error source 链里才有关键信息（证书 vs 连接）
            let mut source = std::error::Error::source(&error);
            while let Some(cause) = source {
                text.push_str(&format!(" ← {cause}"));
                source = cause.source();
            }
            (false, text)
        }
    };
    steps.push(Step {
        name: "tls",
        detail,
        ok,
        took: started.elapsed(),
    });

    steps
}

/// 引擎层自检：完全不碰网络，只回答「QuickJS + bundle + agent boot 行不行」。
fn engine_step() -> Step {
    let started = Instant::now();
    let (ok, detail) = match crate::guest::engine_selftest() {
        Ok(detail) => (true, detail),
        Err(error) => (false, error),
    };
    Step {
        name: "engine",
        detail,
        ok,
        took: started.elapsed(),
    }
}

pub fn run(cfg: &DeepSeekConfig) -> Result<(), String> {
    println!("── net-check（真机自诊断）─────────────────────────────────────");
    println!("base url     {}", cfg.base_url);
    println!("api key      {} 字符", cfg.api_key.len());
    println!();

    let mut failed = Vec::new();
    let mut report = |step: Step| {
        println!(
            "  {:<7} {:<5} {:>7.1} ms  {}",
            step.name,
            if step.ok { "ok" } else { "FAIL" },
            step.took.as_secs_f64() * 1000.0,
            step.detail
        );
        if !step.ok {
            failed.push(step.name);
        }
    };
    // **边跑边打印**：真机上若某一步 panic/卡死，已经完成的行要留在屏幕上。
    for step in run_steps(cfg) {
        report(step);
    }
    report(engine_step());

    println!();
    if failed.is_empty() {
        println!("全部通过 —— 网络与引擎都没问题，可以跑正式那轮。");
        Ok(())
    } else {
        Err(format!(
            "有 {} 项失败：{}。按 net-check 的分层判断是哪一层（dns/tls = 网络，engine = 引擎）。",
            failed.len(),
            failed.join(", ")
        ))
    }
}
