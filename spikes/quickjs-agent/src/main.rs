//! spikes/quickjs-agent —— 「薄 JS + 厚原生」路线的可运行 spike。
//!
//! 验证三件事（对应 docs/POCKET-PI-NOTES.md §4 的 B 方案）：
//!   1. pi-agent-core 的 Agent 类能在 **QuickJS**（rquickjs）里跑；
//!   2. 模型传输在 **Rust** 侧（DeepSeek 一家），JS 里没有 HTTP/provider 栈；
//!   3. 工具直接复用 **现有 Rust 实现**（`pi-host-tools`，与 Tauri 宿主同一份）。
//!
//! 用法：
//!   DEEPSEEK_API_KEY=sk-… cargo run -- --prompt "读一下 notes.md 并追加一行"
//!
//! 输出含三项指标（bundle 体积 / 冷启动 / 首 token 与整轮延迟），见 README。

mod deepseek;
mod guest;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use guest::{Guest, Sink};
use pi_host_tools::HostTools;
use serde_json::Value;

const HARD_TIMEOUT: Duration = Duration::from_secs(180);
const TICK_INTERVAL: Duration = Duration::from_millis(2);

struct Args {
    prompt: String,
    model: String,
    thinking: String,
    workspace: PathBuf,
    quiet: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut prompt = String::new();
    let mut model = std::env::var("SPIKE_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".into());
    let mut thinking = "high".to_string();
    let mut workspace = PathBuf::from("spikes/quickjs-agent/workspace");
    let mut quiet = false;

    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--prompt" => prompt = argv.next().ok_or("--prompt needs a value")?,
            "--model" => model = argv.next().ok_or("--model needs a value")?,
            "--thinking" => thinking = argv.next().ok_or("--thinking needs a value")?,
            "--workspace" => workspace = PathBuf::from(argv.next().ok_or("--workspace needs a value")?),
            "--quiet" => quiet = true,
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if prompt.is_empty() {
        prompt = "Read notes.md, then append a line saying which files you found in the workspace."
            .to_string();
    }
    Ok(Args { prompt, model, thinking, workspace, quiet })
}

/// 播种 workspace：让工具调用有真实对象（也顺带验证 jail 内的读写往返）。
fn seed_workspace(root: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(root.join("src")).map_err(|e| format!("seed dir: {e}"))?;
    let notes = root.join("notes.md");
    if !notes.exists() {
        std::fs::write(
            &notes,
            "# Spike notes\n\nThis workspace belongs to spikes/quickjs-agent.\n",
        )
        .map_err(|e| format!("seed notes: {e}"))?;
    }
    let app = root.join("src/app.js");
    if !app.exists() {
        std::fs::write(&app, "export const hello = () => 'hi';\n")
            .map_err(|e| format!("seed app: {e}"))?;
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("\nFAIL: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args = parse_args()?;
    let cfg = deepseek::DeepSeekConfig::from_env()?;

    // 数据目录放 .data（备份根），与 workspace 分开 —— 与 App 内布局一致。
    let data_dir = PathBuf::from("spikes/quickjs-agent/.data");
    seed_workspace(&args.workspace)?;
    let tools = HostTools::new(&args.workspace, &data_dir);
    let sink = std::sync::Arc::new(Sink::new(cfg));

    let bundle_bytes = std::fs::metadata("spikes/quickjs-agent/dist/agent.js")
        .map(|m| m.len())
        .map_err(|e| {
            format!("dist/agent.js missing ({e}) — run `bash spikes/quickjs-agent/js/build.sh` first")
        })?;

    println!("── quickjs-agent spike ──────────────────────────────────────");
    println!("model        {}", args.model);
    println!("thinking     {}", args.thinking);
    println!("workspace    {}", args.workspace.display());
    println!("bundle       {} bytes", bundle_bytes);

    // ── 冷启动：建 guest（Runtime + prelude + bundle + boot）────────────
    let boot_start = Instant::now();
    let guest = Guest::start(
        tools,
        sink.clone(),
        &args.model,
        "You are a coding agent running inside a QuickJS guest on a mobile device. \
         You have file tools; use them instead of guessing. Answer briefly.",
        &args.thinking,
    )?;
    let boot_ms = boot_start.elapsed();

    println!("\n── 1. boot ──────────────────────────────────────────────────");
    println!("guest boot          {:>8.1} ms", ms(boot_ms));
    let (heap_used, heap_malloc) = guest.memory_usage();
    println!("quickjs heap        {:>8} bytes used / {} malloc", heap_used, heap_malloc);
    println!("tools exposed       {:>8}", guest.tool_count());

    // ── 一整轮：prompt → 工具 → 收尾 ────────────────────────────────────
    let turn_start = Instant::now();
    guest.prompt(&args.prompt)?;
    let mut transcript: Vec<String> = Vec::new();
    let mut tool_calls = 0usize;
    let mut finished = false;
    let mut failure: Option<String> = None;

    while turn_start.elapsed() < HARD_TIMEOUT {
        for event in guest.tick()? {
            match event["type"].as_str() {
                Some("agent_ready") => {}
                Some("message_update") => {
                    if event["kind"] == "text_delta" {
                        if !args.quiet {
                            if let Some(delta) = event["delta"].as_str() {
                                print!("{delta}");
                                use std::io::Write;
                                let _ = std::io::stdout().flush();
                            }
                        }
                    }
                }
                Some("message_end") => {
                    if event["role"] == "assistant" {
                        if event["stopReason"] == "error" {
                            failure = Some(
                                event["errorMessage"].as_str().unwrap_or("model error").to_string(),
                            );
                        } else if !args.quiet {
                            println!();
                        }
                    }
                }
                Some("tool_execution_start") => {
                    tool_calls += 1;
                    transcript.push(format!("tool → {}", event["name"].as_str().unwrap_or("?")));
                    println!("  [tool] {}", event["name"].as_str().unwrap_or("?"));
                }
                Some("tool_execution_end") => {
                    let name = event["name"].as_str().unwrap_or("?");
                    let bad = event["isError"].as_bool().unwrap_or(false);
                    transcript.push(format!("tool ← {name}{}", if bad { " (error)" } else { "" }));
                }
                Some("agent_end") => finished = true,
                Some("agent_error") => {
                    failure = Some(event["message"].as_str().unwrap_or("agent error").to_string());
                    finished = true;
                }
                _ => {}
            }
        }
        if finished {
            break;
        }
        std::thread::sleep(TICK_INTERVAL);
    }

    let turn_ms = turn_start.elapsed();

    println!("\n── 2. turn ──────────────────────────────────────────────────");
    match sink.first_delta() {
        Some(ttft) => println!("prompt → 1st token {:>8.1} ms", ms(ttft)),
        None => println!("prompt → 1st token          —  (no delta)"),
    }
    println!("prompt → turn end   {:>8.1} ms", ms(turn_ms));
    println!("tool calls          {:>8}", tool_calls);
    for (id, span) in sink.model_spans.lock().unwrap().iter() {
        println!("  model request #{id}  {:>8.1} ms", ms(*span));
    }
    for (name, span) in sink.tool_spans.lock().unwrap().iter() {
        println!("  tool {name:<12} {:>8.1} ms", ms(*span));
    }
    let (prompt_tokens, output_tokens) = *sink.tokens.lock().unwrap();
    println!("tokens              {:>8} in / {} out", prompt_tokens, output_tokens);
    let (heap_used, heap_malloc) = guest.memory_usage();
    println!("quickjs heap (end)  {:>8} bytes used / {} malloc", heap_used, heap_malloc);

    if let Some(error) = failure {
        println!("\n── turn failed ──────────────────────────────────────────────");
        return Err(error);
    }
    if !finished {
        return Err(format!("no agent_end within {}s", HARD_TIMEOUT.as_secs()));
    }

    println!("\n── 3. workspace after ───────────────────────────────────────");
    let tree: Value =
        serde_json::from_str(&guest.tools().workspace_tree().map_err(|e| e)?).map_err(|e| e.to_string())?;
    for entry in tree.as_array().cloned().unwrap_or_default() {
        println!(
            "  {:<12} {:>6} B  {}",
            entry["kind"].as_str().unwrap_or("?"),
            entry["size"].as_u64().unwrap_or(0),
            entry["path"].as_str().unwrap_or("?")
        );
    }

    println!("\nOK");
    Ok(())
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
