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

mod approval;
mod ask_user;
mod deepseek;
mod guest;
mod netcheck;

use std::path::PathBuf;
use std::time::{Duration, Instant};

use approval::{Approvals, DecisionSource};
use guest::{Guest, Sink};
use pi_host_tools::HostTools;
use serde_json::Value;

/// 基础 system prompt。AGENTS.md / 目标 / todo 引导由 JS 侧组装（见 composeSystemPrompt），
/// 与 App 的分工一致。
const SYSTEM_PROMPT: &str =
    "You are a coding agent running inside a QuickJS guest on a mobile device. \
     You have file tools; use them instead of guessing. Answer briefly.";

const HARD_TIMEOUT: Duration = Duration::from_secs(180);
const TICK_INTERVAL: Duration = Duration::from_millis(2);

struct Args {
    prompt: String,
    model: String,
    thinking: String,
    workspace: PathBuf,
    data_dir: PathBuf,
    quiet: bool,
    decision: DecisionSource,
    resume: bool,
    goal: Option<String>,
    net_check: bool,
    compact_at: u64,
    mcp_config: Option<PathBuf>,
    plan: Option<String>,
    btw: Option<String>,
    list_sessions: bool,
    open_session: Option<String>,
    auto_continue: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut prompt = String::new();
    let mut model = std::env::var("SPIKE_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".into());
    let mut thinking = "high".to_string();
    let mut workspace = PathBuf::from("spikes/quickjs-agent/workspace");
    // 真机上没有仓库相对路径，Android 跑法用 --workspace/--data-dir 显式给
    let mut data_dir = PathBuf::from("spikes/quickjs-agent/.data");
    let mut quiet = false;
    let mut decision = DecisionSource::Prompt;
    let mut resume = false;
    let mut goal: Option<String> = None;
    let mut net_check = false;
    let mut compact_at = 0u64;
    let mut mcp_config: Option<PathBuf> = None;
    let mut plan: Option<String> = None;
    let mut btw: Option<String> = None;
    let mut list_sessions = false;
    let mut open_session: Option<String> = None;
    let mut auto_continue = true;

    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--prompt" => prompt = argv.next().ok_or("--prompt needs a value")?,
            "--model" => model = argv.next().ok_or("--model needs a value")?,
            "--thinking" => thinking = argv.next().ok_or("--thinking needs a value")?,
            "--workspace" => {
                workspace = PathBuf::from(argv.next().ok_or("--workspace needs a value")?)
            }
            "--data-dir" => {
                data_dir = PathBuf::from(argv.next().ok_or("--data-dir needs a value")?)
            }
            "--quiet" => quiet = true,
            "--resume" => resume = true,
            "--net-check" => net_check = true,
            "--plan" => plan = Some(argv.next().ok_or("--plan needs an objective")?),
            "--btw" => btw = Some(argv.next().ok_or("--btw needs a question")?),
            "--list-sessions" => list_sessions = true,
            "--open-session" => {
                open_session = Some(argv.next().ok_or("--open-session needs an id")?)
            }
            "--no-auto-continue" => auto_continue = false,
            "--mcp-config" => {
                mcp_config = Some(PathBuf::from(
                    argv.next().ok_or("--mcp-config needs a value")?,
                ))
            }
            "--compact-at" => {
                compact_at = argv
                    .next()
                    .ok_or("--compact-at needs a value (tokens)")?
                    .parse()
                    .map_err(|e| format!("--compact-at: {e}"))?;
            }
            "--goal" => goal = Some(argv.next().ok_or("--goal needs a value")?),
            // 审批的三种决策源：交互（默认）/ 全放行 / 全拒绝。后两者也让审批
            // 这条链路可以在无人值守下被验证（仍然走完整握手）。
            "--yes" => decision = DecisionSource::ApproveAll,
            "--deny" => decision = DecisionSource::DenyAll,
            "--delay-approval" => {
                let millis = argv
                    .next()
                    .ok_or("--delay-approval needs a value (milliseconds)")?
                    .parse::<u64>()
                    .map_err(|e| format!("--delay-approval: {e}"))?;
                decision = DecisionSource::Delay(millis);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    if prompt.is_empty() {
        prompt = "Read notes.md, then append a line saying which files you found in the workspace."
            .to_string();
    }
    Ok(Args {
        prompt,
        model,
        thinking,
        workspace,
        data_dir,
        quiet,
        decision,
        resume,
        goal,
        net_check,
        compact_at,
        mcp_config,
        plan,
        btw,
        list_sessions,
        open_session,
        auto_continue,
    })
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
    // AGENTS.md 一并播种：它是「项目指令注入 systemPrompt」这条链路的被测对象，
    // 不播种的话这条路径永远跑不到（与 App 的 refreshAgentsMd 同源）。
    let agents = root.join("AGENTS.md");
    if !agents.exists() {
        std::fs::write(
            &agents,
            "# Project instructions\n\nThis workspace belongs to the quickjs-agent spike.\nKeep every file under 40 lines and prefer editing over rewriting.\n",
        )
        .map_err(|e| format!("seed AGENTS.md: {e}"))?;
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

    // 真机自诊断：只验网络与引擎，不起 agent、不发对话请求
    if args.net_check {
        return netcheck::run(&cfg);
    }

    // 数据目录放 .data（备份根 + policy.json + sessions），与 workspace 分开
    // —— 与 App 内布局一致。
    let data_dir = args.data_dir.clone();
    seed_workspace(&args.workspace)?;
    let tools = HostTools::new(&args.workspace, &data_dir);
    let sink = std::sync::Arc::new(Sink::new(cfg));
    let asks = std::sync::Arc::new(ask_user::AskUser::new(args.decision, sink.clone()));
    let approvals = std::sync::Arc::new(Approvals::new(
        &args.workspace,
        &data_dir,
        args.decision,
        sink.clone(),
    ));

    // bundle 已编译进二进制（include_str!），运行期不依赖 dist/agent.js —— 真机上
    // 没有那个路径（早先这里读文件只为打印体积，Android 上直接失败）。
    let bundle_bytes = guest::bundle_bytes();

    println!("── quickjs-agent spike ──────────────────────────────────────");
    println!("model        {}", args.model);
    println!("thinking     {}", args.thinking);
    println!("workspace    {}", args.workspace.display());
    match args.decision {
        DecisionSource::Prompt => println!("approvals    prompt (终端交互)"),
        DecisionSource::ApproveAll => println!("approvals    auto-allow (--yes)"),
        DecisionSource::DenyAll => println!("approvals    auto-deny (--deny)"),
        DecisionSource::Delay(ms) => println!("approvals    delayed {ms} ms then allow"),
    }
    println!("bundle       {} bytes", bundle_bytes);

    // ── 冷启动：建 guest（Runtime + prelude + bundle + boot）────────────
    let boot_start = Instant::now();
    let sessions_dir = data_dir.join("sessions");
    let goal_path = data_dir.join("goal.json");
    // 会话根目录由**宿主**建（App 里是 lib.rs 启动时 create_dir_all 的 ["sessions",
    // "workspace"] 之一）。抽出来的 fs_op 里 createDir 默认**非递归**，所以这条宿主
    // 职责不能省 —— 第一版就是漏了它，`repo.create` 直接 ENOENT、会话静默没落盘。
    std::fs::create_dir_all(&sessions_dir).map_err(|e| format!("sessions dir: {e}"))?;
    // 目标由**宿主持有**（goal.json），JS 只负责拼进 systemPrompt —— 与 App 同一分工。
    if let Some(objective) = &args.goal {
        if let Some(parent) = goal_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(
            &goal_path,
            serde_json::json!({ "objective": objective }).to_string(),
        )
        .map_err(|e| format!("goal write: {e}"))?;
    }
    let workspace_label = args
        .workspace
        .canonicalize()
        .unwrap_or_else(|_| args.workspace.clone());
    // MCP 服务器的授权源：从宿主自己那份配置读出 `scheme://host:port`。
    // SSRF 防护对 fetch 必须严格，但用户显式配置的 MCP 目标（常在本地/局域网）
    // 应当被授权 —— 判据是「配置过」而不是「放宽防护」。
    let mcp_config_path = args
        .mcp_config
        .clone()
        .unwrap_or_else(|| data_dir.join("mcp.json"));
    let allowed_origins = mcp_server_origins(&mcp_config_path);
    if !allowed_origins.is_empty() {
        println!("mcp origins  {}", allowed_origins.join(" "));
    }

    let guest = Guest::start(
        guest::HostDeps {
            tools,
            sink: sink.clone(),
            approvals: approvals.clone(),
            asks,
            sessions_root: sessions_dir.clone(),
            goal_path: goal_path.clone(),
            mcp_config_path: mcp_config_path.clone(),
            allowed_origins: allowed_origins.clone(),
            data_dir: data_dir.clone(),
        },
        guest::GuestOptions {
            model_label: args.model.clone(),
            system_prompt: SYSTEM_PROMPT.to_string(),
            thinking_level: args.thinking.clone(),
            workspace_label: workspace_label.to_string_lossy().into_owned(),
            compact_at: args.compact_at,
        },
    )?;
    let boot_ms = boot_start.elapsed();

    println!("\n── 1. boot ──────────────────────────────────────────────────");
    println!("guest boot          {:>8.1} ms", ms(boot_ms));
    let (heap_used, heap_malloc) = guest.memory_usage();
    println!(
        "quickjs heap        {:>8} bytes used / {} malloc",
        heap_used, heap_malloc
    );
    if let Ok(info) = guest.session_info() {
        if let Some(goal) = info["goal"].as_str() {
            println!("goal                {}", goal);
        }
    }

    // ── 等 context_ready：AGENTS.md 是异步读的（要过宿主握手），别抢在它前面 prompt
    {
        let mut ready = false;
        for _ in 0..500 {
            for event in guest.tick()? {
                match event["type"].as_str() {
                    Some("context_ready") => ready = true,
                    Some("agents_md_loaded") => {
                        println!(
                            "AGENTS.md           {} bytes",
                            event["bytes"].as_u64().unwrap_or(0)
                        )
                    }
                    Some("session_error") => {
                        println!("  [session] {}", event["error"].as_str().unwrap_or("?"))
                    }
                    Some("skills_applied") => println!(
                        "skills              {} 个已注入 systemPrompt",
                        event["count"].as_u64().unwrap_or(0)
                    ),
                    Some("mcp_connecting") => {
                        println!("  [mcp] 连接 {} …", event["server"].as_str().unwrap_or("?"))
                    }
                    Some("mcp_ready") => println!(
                        "  [mcp] {} 就绪：{}",
                        event["server"].as_str().unwrap_or("?"),
                        event["tools"]
                            .as_array()
                            .map(|list| list
                                .iter()
                                .filter_map(|t| t.as_str())
                                .collect::<Vec<_>>()
                                .join(" "))
                            .unwrap_or_default()
                    ),
                    Some("mcp_error") => println!(
                        "  [mcp] {} 失败：{}",
                        event["server"].as_str().unwrap_or("?"),
                        event["error"].as_str().unwrap_or("?")
                    ),
                    Some("mcp_tools_registered") => println!(
                        "  [mcp] 共注册 {} 个 mcp__ 工具",
                        event["count"].as_u64().unwrap_or(0)
                    ),
                    _ => {}
                }
            }
            if ready {
                break;
            }
            std::thread::sleep(TICK_INTERVAL);
        }
        if !ready {
            return Err("context_ready 未到达（systemPrompt 可能没装完）".into());
        }
    }
    // 工具清单从 guest 取（对齐 App 的 __pi_tool_names）。**放在 context_ready 之后**：
    // MCP 工具是那期间异步注册的，boot 时就打印会漏掉它们。
    let tools = guest.tool_names()?;
    println!(
        "tools exposed       {:>8}  {}",
        tools.len(),
        tools.join(" ")
    );

    // ── 会话恢复（--resume）：把最新会话的消息灌回 agent ────────────────
    if args.resume {
        let restore_start = Instant::now();
        guest.restore()?;
        // restore 是异步的（走 fs hostcall），跟着 tick 循环把事件推完
        let mut restored = false;
        for _ in 0..500 {
            for event in guest.tick()? {
                match event["type"].as_str() {
                    Some("restore_done") => {
                        let info = guest.session_info()?;
                        println!(
                            "resumed session     {} ({} messages) in {:.1} ms",
                            info["sessionId"].as_str().unwrap_or("?"),
                            info["restoredMessages"].as_u64().unwrap_or(0),
                            ms(restore_start.elapsed())
                        );
                        restored = true;
                    }
                    Some("session_error") => {
                        return Err(event["error"]
                            .as_str()
                            .unwrap_or("restore failed")
                            .to_string())
                    }
                    _ => {}
                }
            }
            if restored {
                break;
            }
            std::thread::sleep(TICK_INTERVAL);
        }
        if !restored {
            return Err("session restore did not complete (no session yet?)".into());
        }
    }

    // ── 只做一件事的模式（命令面）：跑完就退，不进对话循环 ──────────────
    if args.list_sessions {
        println!("\n── sessions ─────────────────────────────────────────────────");
        guest.list_sessions()?;
        // kick 模式：等 session_list 事件（与 restore 同形）
        let mut sessions: Option<Value> = None;
        let started = Instant::now();
        while sessions.is_none() && started.elapsed() < Duration::from_secs(30) {
            for event in guest.tick()? {
                match event["type"].as_str() {
                    Some("session_list") => sessions = Some(event["sessions"].clone()),
                    Some("session_error") => {
                        return Err(event["error"].as_str().unwrap_or("list failed").to_string())
                    }
                    _ => {}
                }
            }
            std::thread::sleep(TICK_INTERVAL);
        }
        let list = sessions.ok_or("session_list 未到达")?;
        for (index, meta) in list
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .enumerate()
        {
            println!(
                "  {}{:<38} modified {}",
                if index == 0 { "* " } else { "  " },
                meta["id"].as_str().unwrap_or("?"),
                meta["modifiedAt"].as_u64().unwrap_or(0)
            );
        }
        println!("  （* = 最新；--open-session <id> 打开）");
        return Ok(());
    }
    if let Some(id) = &args.open_session {
        guest.open_session(id)?;
        // 打开是异步的（走 fs hostcall），跟着 tick 推完
        for _ in 0..500 {
            let events = guest.tick()?;
            if events.iter().any(|e| e["type"] == "session_opened") {
                break;
            }
            std::thread::sleep(TICK_INTERVAL);
        }
        let info = guest.session_info()?;
        println!(
            "opened session      {} ({} messages)",
            info["sessionId"].as_str().unwrap_or("?"),
            info["restoredMessages"].as_u64().unwrap_or(0)
        );
        return Ok(());
    }
    if let Some(objective) = &args.plan {
        println!("\n── plan ─────────────────────────────────────────────────────");
        guest.plan(objective)?;
        let text = collect_nested(&guest, "plan_drafted", "plan_error")?;
        println!("{text}");
        return Ok(());
    }
    if let Some(question) = &args.btw {
        println!("\n── btw ──────────────────────────────────────────────────────");
        guest.btw(question)?;
        let text = collect_nested(&guest, "btw_answer", "btw_error")?;
        println!("{text}");
        return Ok(());
    }
    guest.set_auto_continue(args.auto_continue)?;

    // prompt 前的水位 —— 压缩阈值就是拿它判的，所以它必须可见
    if let Ok(status) = guest.status() {
        println!(
            "context before      {} tokens / {} 阈值",
            status["contextTokens"].as_u64().unwrap_or(0),
            status["compactThreshold"].as_u64().unwrap_or(0)
        );
    }

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
                    if event["kind"] == "text_delta" && !args.quiet {
                        if let Some(delta) = event["delta"].as_str() {
                            print!("{delta}");
                            use std::io::Write;
                            let _ = std::io::stdout().flush();
                        }
                    }
                }
                Some("message_end") => {
                    if event["role"] == "assistant" {
                        if event["stopReason"] == "error" {
                            failure = Some(
                                event["errorMessage"]
                                    .as_str()
                                    .unwrap_or("model error")
                                    .to_string(),
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
                    transcript.push(format!(
                        "tool ← {name}{}",
                        if bad { " (error)" } else { "" }
                    ));
                }
                Some("approval_request") => {
                    println!(
                        "  [approval] {} ({}) — {}",
                        event["tool"].as_str().unwrap_or("?"),
                        event["tier"].as_str().unwrap_or("?"),
                        event["summary"].as_str().unwrap_or("")
                    );
                }
                Some("compaction_check") => println!(
                    "  [compact] check: 水位 {} / 阈值 {} / {} 条消息",
                    event["tokens"].as_u64().unwrap_or(0),
                    event["threshold"].as_u64().unwrap_or(0),
                    event["messages"].as_u64().unwrap_or(0)
                ),
                Some("compaction_step") => println!(
                    "  [compact] step: {}",
                    event["step"].as_str().unwrap_or("?")
                ),
                Some("compaction_start") => println!(
                    "  [compact] 开始：上下文 {} tokens / {} 条消息",
                    event["tokens"].as_u64().unwrap_or(0),
                    event["messages"].as_u64().unwrap_or(0)
                ),
                Some("compaction_done") => println!(
                    "  [compact] 完成：摘要 {} 条，保留最近 {} 条",
                    event["summarized"].as_u64().unwrap_or(0),
                    event["kept"].as_u64().unwrap_or(0)
                ),
                Some("subagent_start") => println!(
                    "  [subagent] {} ← {}",
                    event["name"].as_str().unwrap_or("?"),
                    event["task"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(64)
                        .collect::<String>()
                ),
                Some("ask_user") => println!(
                    "  [ask_user] {}  ({} 个选项)",
                    event["question"].as_str().unwrap_or("?"),
                    event["options"].as_array().map(|a| a.len()).unwrap_or(0)
                ),
                Some("subagent_end") => {
                    println!(
                        "  [subagent] {} 完成",
                        event["name"].as_str().unwrap_or("?")
                    )
                }
                Some("approval_resolved") => {
                    println!(
                        "  [approval] {} → {} ({})",
                        event["tool"].as_str().unwrap_or("?"),
                        event["decision"].as_str().unwrap_or("?"),
                        event["reason"].as_str().unwrap_or("")
                    );
                }
                Some("session_created") => {
                    println!(
                        "  [session] created {}",
                        event["sessionId"].as_str().unwrap_or("?")
                    );
                }
                Some("todo_updated") => {
                    let tasks = event["tasks"].as_array().cloned().unwrap_or_default();
                    let open = tasks.iter().filter(|t| t["status"] != "deleted").count();
                    let active = tasks
                        .iter()
                        .find(|t| t["status"] == "in_progress")
                        .and_then(|t| t["subject"].as_str())
                        .unwrap_or("-");
                    println!("  [todo] {open} open, in_progress: {active}");
                }
                Some("session_error") => {
                    println!(
                        "  [session] error: {}",
                        event["error"].as_str().unwrap_or("?")
                    );
                }
                // **不是** agent_end 就收工：goal 存续时会自动续跑，终态由 agent_idle 给出
                Some("agent_idle") => finished = true,
                Some("goal_auto_continue") => println!(
                    "  [goal] 自动续跑 {}/{}",
                    event["count"].as_u64().unwrap_or(0),
                    event["cap"].as_u64().unwrap_or(0)
                ),
                Some("goal_auto_done") => println!("  [goal] 模型报告 GOAL_COMPLETE，停止续跑"),
                Some("goal_error") => {
                    println!("  [goal] {}", event["error"].as_str().unwrap_or("?"))
                }
                Some("agent_error") => {
                    failure = Some(
                        event["message"]
                            .as_str()
                            .unwrap_or("agent error")
                            .to_string(),
                    );
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
    let denied = sink.denied_calls.lock().unwrap().clone();
    for name in &denied {
        println!("  tool {name:<12}     denied (审批拦住，未执行)");
    }
    let (prompt_tokens, output_tokens) = *sink.tokens.lock().unwrap();
    println!(
        "tokens              {:>8} in / {} out",
        prompt_tokens, output_tokens
    );
    let waited = sink.ticks_while_waiting();
    if waited > 0 {
        // 这条数字的意义：审批在等用户时，guest 线程仍在跑 —— 真机上这就是
        // 「等审批时 UI 不卡死」的那个性质（M1 在 VM 线程上等 I/O 会死锁）。
        println!("ticks while waiting {:>8}  (guest 未被 stdin 阻塞)", waited);
    }
    let (heap_used, heap_malloc) = guest.memory_usage();
    println!(
        "quickjs heap (end)  {:>8} bytes used / {} malloc",
        heap_used, heap_malloc
    );

    if let Some(error) = failure {
        println!("\n── turn failed ──────────────────────────────────────────────");
        return Err(error);
    }
    if !finished {
        return Err(format!("no agent_end within {}s", HARD_TIMEOUT.as_secs()));
    }

    println!("\n── 3. session ───────────────────────────────────────────────");
    let info = guest.session_info()?;
    println!(
        "session id          {}",
        info["sessionId"].as_str().unwrap_or("(none)")
    );
    println!(
        "messages in agent   {}",
        info["messages"].as_u64().unwrap_or(0)
    );
    println!(
        "todos tracked       {}",
        info["todos"].as_u64().unwrap_or(0)
    );
    let status = guest.status()?;
    println!(
        "context tokens      {} / {} (compact 阈值)",
        status["contextTokens"].as_u64().unwrap_or(0),
        status["compactThreshold"].as_u64().unwrap_or(0)
    );
    println!("session files       {}", sessions_dir.display());

    println!("\n── 4. workspace after ───────────────────────────────────────");
    let tree: Value =
        serde_json::from_str(&guest.tools().workspace_tree()?).map_err(|e| e.to_string())?;
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

/// 从 mcp.json 里取所有服务器的源（给 `http` 通道的授权列表用）。
fn mcp_server_origins(path: &std::path::Path) -> Vec<String> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(config) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    config["servers"]
        .as_array()
        .map(|servers| {
            servers
                .iter()
                .filter_map(|server| server["url"].as_str())
                .filter_map(pi_host_tools::http::origin_of)
                .collect()
        })
        .unwrap_or_default()
}

/// 等一个「嵌套 run」的结果事件（plan / btw 用）。终态是 xxx_drafted/xxx_answer 或
/// xxx_error —— 与 App 的 kick+事件模式同形。
fn collect_nested(guest: &Guest, done: &str, failed: &str) -> Result<String, String> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(120) {
        for event in guest.tick()? {
            match event["type"].as_str() {
                Some(kind) if kind == done => {
                    return Ok(event["content"]
                        .as_str()
                        .or_else(|| event["answer"].as_str())
                        .unwrap_or("(no output)")
                        .to_string())
                }
                Some(kind) if kind == failed => {
                    return Err(event["error"]
                        .as_str()
                        .unwrap_or("nested run failed")
                        .to_string())
                }
                _ => {}
            }
        }
        std::thread::sleep(TICK_INTERVAL);
    }
    Err(format!("{done} 未到达（120s 超时）"))
}

fn ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}
