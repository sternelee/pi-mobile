import { createMediaQuery } from "@solid-primitives/media";
import { makePersisted } from "@solid-primitives/storage";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  createMemo,
  createSignal,
  onCleanup,
  onMount,
  type Signal,
} from "solid-js";
import { ApprovalCard } from "~/components/ApprovalCard";
import { AskUserCard } from "~/components/AskUserCard";
import { ChatStream } from "~/components/ChatStream";
import { CommandPalette } from "~/components/CommandPalette";
import { Composer } from "~/components/Composer";
import { FilePreviewDialog } from "~/components/FilePreviewDialog";
import { GoalBanner } from "~/components/GoalBanner";
import { ModelPicker } from "~/components/ModelPicker";
import { PlanCard } from "~/components/PlanCard";
import { PreviewPanel } from "~/components/PreviewPanel";
import { ProviderSetup } from "~/components/ProviderSetup";
import { QuickBar } from "~/components/QuickBar";
import { SessionDrawer } from "~/components/SessionDrawer";
import { SettingsSheet } from "~/components/SettingsSheet";
import { TodoPanel } from "~/components/TodoPanel";
import { TopBar } from "~/components/TopBar";
import { WorkspaceDrawer } from "~/components/WorkspaceDrawer";
import {
  type AgentHistoryMessage,
  type AgentHistoryResponse,
  parsePiEvent,
  type ToolResultPayload,
} from "~/lib/events";
import { allCommands, refreshSkillCmds, skillCmds } from "~/lib/providers";
import type {
  Approval,
  AskRequest,
  ChatItem,
  NativeCapability,
  SettingsView,
  TodoTask,
} from "~/lib/types";
import { useMcp } from "~/state/mcp";
import { usePreview } from "~/state/preview";
import { useProviders } from "~/state/providers";
import { useSessions } from "~/state/sessions";
import { useSkills } from "~/state/skills";
import type { TreeEntry } from "~/ui/WorkspaceTree";
import "./App.css";

function App() {
  const [items, setItems] = createSignal<ChatItem[]>([
    { role: "status", text: "booting embedded pi agent…" },
  ]);
  const [input, setInput] = makePersisted<string, Signal<string>>(
    createSignal(""),
    {
      name: "pi-draft",
    },
  );
  const [ready, setReady] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [approval, setApproval] = createSignal<Approval | null>(null);

  // D14：脚本审批卡上的能力说明。**单一真源在 Rust 的 `script.rs`** —— 这里
  // 只缓存 id → 说明的映射，绝不在 TS 另写一份说明文字：两份一旦不一致，
  // 用户看到的就是与实际授权集不同的东西，而审批卡的全部意义是「所见即所授」。
  const [capCatalog, setCapCatalog] = createSignal<Record<string, string>>({});
  const loadCapCatalog = async () => {
    if (Object.keys(capCatalog()).length) return;
    try {
      const c = await invoke<{ grantable: { id: string; desc: string }[] }>(
        "script_capabilities",
      );
      const map: Record<string, string> = {};
      for (const g of c.grantable ?? []) map[g.id] = g.desc;
      setCapCatalog(map);
    } catch {
      // 取不到就退回显示 id：卡片仍可用 —— 绝不因为文案缺失而挡住审批。
    }
  };
  const capLabel = (id: string) => capCatalog()[id] ?? id;

  const [ask, setAsk] = createSignal<AskRequest | null>(null);
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [filesOpen, setFilesOpen] = createSignal(false);
  const [tree, setTree] = createSignal<TreeEntry[]>([]);
  const [preview, setPreview] = createSignal<{
    path: string;
    content: string;
  } | null>(null);
  const [stick, setStick] = createSignal(true);
  const [sessionTokens, setSessionTokens] = createSignal(0);
  const [plan, setPlan] = createSignal<{
    objective: string;
    content: string;
  } | null>(null);
  const [planning, setPlanning] = createSignal(false);
  const [goal, setGoal] = createSignal<string | null>(null);
  // autoContinue 状态（goal_auto_continue 事件驱动；null = 本回合非自动续跑）
  const [goalAuto, setGoalAuto] = createSignal<{
    count: number;
    cap: number;
  } | null>(null);
  const [todos, setTodos] = createSignal<{ tasks: TodoTask[]; nextId: number }>(
    {
      tasks: [],
      nextId: 1,
    },
  );
  const [todoOpen, setTodoOpen] = createSignal(false);

  // ── UI 2.0：动效偏好 / 会话搜索 / 设置页导航（@solid-primitives）──
  const reducedMotion = createMediaQuery("(prefers-reduced-motion: reduce)");
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [settingsView, setSettingsView] =
    createSignal<SettingsView>("providers");
  const [nativeCaps, setNativeCaps] = createSignal<NativeCapability[]>([]);
  const [nativeErr, setNativeErr] = createSignal("");
  async function refreshNativeCaps() {
    try {
      const v = await invoke<{ capabilities: NativeCapability[] }>(
        "native_capabilities",
      );
      setNativeCaps(v.capabilities ?? []);
      setNativeErr("");
    } catch (e) {
      setNativeErr(String(e));
    }
  }
  async function requestNativePermission(cap: string) {
    try {
      await invoke("native_request_permission", { capability: cap });
    } catch (e) {
      push({ role: "status", text: `permission request failed: ${e}` });
    }
    // 系统弹窗是异步的，留一点时间让用户在弹窗上作答后再拉状态
    setTimeout(() => void refreshNativeCaps(), 600);
  }
  // 审批策略（write 基线 ask/auto）——Agent 设置页的开关
  const [approvalPolicy, setApprovalPolicy] = createSignal<"ask" | "auto">(
    "ask",
  );
  const setApprovalPolicyPersist = async (next: "ask" | "auto") => {
    const prev = approvalPolicy();
    setApprovalPolicy(next);
    try {
      await invoke("approval_policy_set", { policy: next });
      push({
        role: "status",
        text:
          next === "ask"
            ? "approval required for file changes (write/edit/mkdir)"
            : "file changes run without approval",
      });
    } catch (e) {
      setApprovalPolicy(prev);
      push({ role: "status", text: `approval_policy_set failed: ${e}` });
    }
  };

  let textareaEl: HTMLTextAreaElement | undefined;

  const push = (item: ChatItem) => setItems((prev) => [...prev, item]);
  const updateItem = (toolCallId: string, patch: Partial<ChatItem>) =>
    setItems((prev) =>
      prev.map((it) =>
        it.toolCallId === toolCallId ? { ...it, ...patch } : it,
      ),
    );

  // 工具执行计时：start 记时间戳，end 算出 durationMs 一并 patch 进卡片
  const toolStartTimes = new Map<string, number>();

  const prov = useProviders(push);
  const mcp = useMcp(push);
  const skills = useSkills(push);
  const sess = useSessions(push, {
    closeDrawer: () => setDrawerOpen(false),
    loadHistory: () => loadHistory(),
    resetChat: (text, resetTokens) => {
      setItems([{ role: "status", text }]);
      if (resetTokens) setSessionTokens(0);
    },
  });
  const previewState = usePreview();

  function mapHistoryMessage(m: AgentHistoryMessage): ChatItem {
    const text =
      typeof m.content === "string"
        ? m.content
        : m.content
            .filter((c) => c.type === "text")
            .map((c) => c.text)
            .join("");
    if (m.role === "toolResult") {
      return { role: "tool", text: `↳ ${text.slice(0, 200)}` };
    }
    if (m.role === "assistant") {
      const calls = m.content
        .filter((c) => c.type === "toolCall")
        .map((c) => `⚒ ${c.name}(${JSON.stringify(c.arguments ?? {})})`);
      return { role: "assistant", text: text || calls.join("\n") };
    }
    return { role: "user", text };
  }

  async function loadHistory() {
    const h = JSON.parse(
      await invoke<string>("agent_history"),
    ) as AgentHistoryResponse;
    sess.setCurrentSession(h.sessionId ?? null);
    const msgs = h.messages ?? [];
    if (!msgs.length) {
      setItems([]);
      return;
    }
    setItems(msgs.map(mapHistoryMessage));
    const total = msgs.reduce((acc: number, m) => {
      const u = "usage" in m ? m.usage : undefined;
      return (
        acc + (u ? (u.totalTokens ?? (u.input ?? 0) + (u.output ?? 0)) : 0)
      );
    }, 0);
    setSessionTokens(total);
    push({ role: "status", text: `history loaded — ${msgs.length} messages` });
  }

  async function refreshCanRevert(path: string) {
    const info = await invoke<string>("workspace_backup_info", { path });
    const has = info !== "null";
    setItems((prev) =>
      prev.map((it) =>
        it.path === path && it.role === "tool" ? { ...it, canRevert: has } : it,
      ),
    );
  }

  async function revert(it: ChatItem) {
    if (!it.path || !it.toolCallId) return;
    try {
      await invoke("workspace_revert", { path: it.path });
      await refreshCanRevert(it.path);
      updateItem(it.toolCallId, { reverted: true });
      push({
        role: "status",
        text: `reverted ${it.path} to the previous version`,
      });
    } catch (e) {
      push({ role: "status", text: `revert failed: ${e}` });
    }
  }

  // DEV 演示种子：`#demo` 打开时注入代表性聊天流（工具卡三态 / markdown /
  // 用户气泡），无 Tauri 后端也能在浏览器里预览 UI —— 保留供后续 UI 迭代做视觉验证
  if (import.meta.env.DEV && window.location.hash === "#demo") {
    setReady(true);
    prov.setCurrentModel({
      provider: "demo",
      id: "V4 Flash",
      name: "V4 Flash",
    });
    setItems([
      { role: "user", text: "Read the workspace README and summarize it" },
      // thinking 空泡：模型先思考再输出（turn_end 前非末尾，不会被清掉）
      { role: "assistant", text: "", thinking: true },
      {
        role: "assistant",
        text: "The workspace contains three files — `backend.sh`, `index.html`, and `initial.json`. There's no README.md, so I'll read an existing file instead (`index.html`):",
      },
      {
        role: "tool",
        toolCallId: "demo-1",
        toolName: "list_dir",
        argsText: '{"path": "."}',
        pending: false,
        durationMs: 843,
        text: "↳ backend.sh\nindex.html\ninitial.json",
      },
      {
        role: "assistant",
        text: "Done. Here's a summary of what happened:\n\n**Two things to flag:**\n\n1. The absolute path was rejected — `/workspace/README.md` failed with `E_AGENT_BAD_PATH`.\n2. `README.md` doesn't exist yet.",
        streaming: true,
      },
      {
        role: "tool",
        toolCallId: "demo-2",
        toolName: "read_file",
        argsText: '{"path": "index.html"}',
        pending: false,
        durationMs: 1695,
        text: "↳ <!doctype html>…",
      },
      {
        role: "tool",
        toolCallId: "demo-3",
        toolName: "write_file",
        argsText: '{"path": "README.md", "content": "# demo\\n"}',
        text: "",
        pending: true,
        path: "README.md",
      },
      {
        role: "tool",
        toolCallId: "demo-4",
        toolName: "edit_file",
        argsText: '{"path": "backend.sh"}',
        pending: false,
        isError: true,
        expanded: true,
        durationMs: 312,
        text: "↳ E_WORKSPACE_LOCKED: file is locked by another tool call",
        path: "backend.sh",
        canRevert: true,
      },
      { role: "status", text: "demo seed — visual verification only" },
    ]);
  }

  onMount(async () => {
    // 持久化的模型选择先回显（agent_ready 后 __pi_model_current 会校正 name）
    try {
      const raw = await invoke<string>("get_default_model");
      const sel = JSON.parse(raw);
      if (sel?.provider) {
        prov.setCurrentModel({
          provider: sel.provider,
          id: sel.modelId,
          name: sel.modelId,
        });
      }
    } catch {
      // 未配置：首启卡片引导选择
    }
    const un = await listen<string>("pi-agent-event", (e) => {
      const ev = parsePiEvent(e.payload);
      if (!ev) return;
      switch (ev.type) {
        case "agent_ready":
          setReady(true);
          // 目录与当前模型回读（runtime 就绪后才有意义）
          prov.refreshProviders();
          refreshSkillCmds();
          invoke<string>("pi_call_global", {
            fnName: "__pi_model_current",
            arg: "",
          })
            .then((r) => {
              const m = JSON.parse(r);
              if (m?.id) prov.setCurrentModel(m);
            })
            .catch(() => {});
          break;
        case "agent_start":
          setBusy(true);
          break;
        case "agent_end":
          setBusy(false);
          break;
        case "agent_error":
          setBusy(false);
          push({ role: "status", text: `ERROR: ${ev.error ?? "unknown"}` });
          break;
        case "session_restored":
          sess.setCurrentSession(ev.sessionId ?? null);
          break;
        case "session_created":
          sess.setCurrentSession(ev.sessionId ?? null);
          break;
        case "session_error":
          push({ role: "status", text: `session persist error: ${ev.error}` });
          break;
        case "approval_required":
          void loadCapCatalog();
          setApproval({
            requestId: ev.requestId,
            tool: ev.tool,
            path: ev.path,
            diff: ev.diff ?? "",
            capabilities: ev.capabilities ?? [],
            code: ev.code ?? "",
            script: ev.script === true,
          });
          break;
        case "preview_open":
          // D15：agent 自己打开了预览（它调了 preview 工具）。面板已开时是
          // **切换 + 重载**——正是「改完再调一次」的迭代循环需要的行为。
          if (typeof ev.path === "string" && ev.path) {
            previewState.setPreviewPath(ev.path);
            previewState.setPreviewList((prev) =>
              prev.includes(ev.path) ? prev : [...prev, ev.path],
            );
            if (typeof ev.port === "number")
              previewState.setPreviewPort(ev.port);
            previewState.setPreviewErr(null);
            previewState.setPreviewNonce((n) => n + 1);
            previewState.setPreviewOpen(true);
          }
          break;
        case "ask_user":
          setAsk({
            requestId: ev.requestId,
            question: ev.question ?? "",
            context: ev.context,
            options: ev.options ?? [],
            allowMultiple: Boolean(ev.allowMultiple),
            allowFreeform: ev.allowFreeform !== false,
            allowComment: Boolean(ev.allowComment),
            selected: [],
            freeform: "",
            comment: "",
          });
          break;
        case "skills_applied":
          // 技能增删/启停后命令面板跟随（__pi_commands 回读）
          refreshSkillCmds();
          break;
        case "oauth_open_url":
          push({
            role: "status",
            text: `browser opened — complete ${ev.provider ?? "provider"} sign-in`,
          });
          break;
        case "oauth_device_code":
          push({
            role: "status",
            text: `enter code ${ev.userCode ?? ""} at ${ev.verificationUri ?? "the verification page"}`,
          });
          break;
        case "oauth_progress":
          push({ role: "status", text: `oauth: ${ev.message ?? "working…"}` });
          break;
        case "oauth_done":
          if (ev.error) {
            push({ role: "status", text: `oauth FAILED: ${ev.error}` });
          } else {
            push({
              role: "status",
              text: `oauth: signed in to ${ev.provider ?? "provider"} — pick a model`,
            });
            void prov.refreshConfigured();
            invoke<string>("pi_call_global", {
              fnName: "__pi_providers_list",
              arg: "",
            }).catch(() => {});
          }
          break;
        case "mcp_ready":
          mcp.setMcpReady((prev) => new Set(prev).add(ev.server));
          push({
            role: "status",
            text: `mcp ${ev.server} ready — ${(ev.tools ?? []).length} tools`,
          });
          break;
        case "mcp_error":
          mcp.setMcpReady((prev) => {
            const next = new Set(prev);
            next.delete(ev.server);
            return next;
          });
          push({ role: "status", text: `mcp ${ev.server}: ${ev.error}` });
          break;
        case "compaction_start":
          push({
            role: "status",
            text: `compacting context (${ev.tokens} tokens, ${ev.messages} messages)…`,
          });
          break;
        case "compaction_done":
          push({
            role: "status",
            text: `context compacted — ${ev.summarized} messages summarized`,
          });
          break;
        case "mcp_tools_registered":
          push({ role: "status", text: `mcp tools registered (${ev.count})` });
          break;
        case "plan_drafted":
          setPlanning(false);
          setPlan({ objective: ev.objective, content: ev.content ?? "" });
          break;
        case "plan_error":
          setPlanning(false);
          push({ role: "status", text: `plan failed: ${ev.error}` });
          break;
        case "btw_thinking":
          push({ role: "status", text: `btw: ${ev.question} — thinking…` });
          break;
        case "btw_answer":
          push({
            role: "assistant",
            text: `💬 ${ev.question}\n\n${ev.answer ?? ""}`,
          });
          break;
        case "btw_error":
          push({ role: "status", text: `btw failed: ${ev.error}` });
          break;
        case "subagent_start":
          push({ role: "status", text: `delegating to ${ev.name}…` });
          break;
        case "subagent_end":
          push({ role: "status", text: `${ev.name} finished` });
          break;
        case "todo_updated": {
          // rpiv-todo 移动原生化：列表非空自动弹面板，清空自动收起
          const tasks: TodoTask[] = ev.tasks ?? [];
          setTodos({ tasks, nextId: ev.nextId ?? 1 });
          setTodoOpen(tasks.length > 0);
          break;
        }
        case "providers_listed":
          prov.setProviders(ev.providers ?? []);
          break;
        case "providers_error":
          push({ role: "status", text: `providers: ${ev.error}` });
          break;
        case "models_listed":
          prov.setLoadingModels(false);
          prov.setProviders((ps) =>
            ps.map((p) =>
              p.id === ev.provider ? { ...p, models: ev.models ?? [] } : p,
            ),
          );
          break;
        case "models_error":
          prov.setLoadingModels(false);
          push({ role: "status", text: `models ${ev.provider}: ${ev.error}` });
          break;
        case "model_applied":
          prov.setCurrentModel({
            provider: ev.provider,
            id: ev.modelId,
            name: ev.name ?? ev.modelId,
          });
          break;
        case "goal_auto_continue":
          setGoalAuto({ count: ev.count ?? 0, cap: ev.cap ?? 10 });
          break;
        case "goal_auto_done":
          setGoalAuto(null);
          push({
            role: "status",
            text: "goal achieved — auto-continue stopped",
          });
          break;
        case "goal_error":
          setGoalAuto(null);
          push({
            role: "status",
            text: `goal auto-continue failed: ${ev.error}`,
          });
          break;
        case "boot_error":
          setBusy(false);
          push({ role: "status", text: `BOOT ERROR: ${ev.error}` });
          break;
        case "turn_end":
          // 回合收束：清掉滞留的 thinking 空泡；带文本的末条 assistant
          // 顺手收掉 streaming 光标（兜底，正常由 message_end 关闭）
          setItems((prev) => {
            const last = prev[prev.length - 1];
            if (last?.role === "assistant" && last.thinking && !last.text)
              return prev.slice(0, -1);
            if (last?.role === "assistant" && last.streaming)
              return [...prev.slice(0, -1), { ...last, streaming: false }];
            return prev;
          });
          break;
        case "message_start":
        case "message_update":
        case "message_end": {
          // 只渲染 assistant 流（user/toolResult 的消息事件另行处理/已在界面）
          const msg = ev.message;
          if (msg.role !== "assistant") break;
          const blocks = msg.content;
          const text = blocks
            .filter((c) => c.type === "text")
            .map((c) => c.text)
            .join("");
          if (
            ev.type === "message_end" &&
            msg.role === "assistant" &&
            msg.usage
          ) {
            const u = msg.usage;
            setSessionTokens(
              (prev) =>
                prev + (u.totalTokens ?? (u.input ?? 0) + (u.output ?? 0)),
            );
          }
          const thinking = blocks.some((c) => c.type === "thinking");
          if (ev.type === "message_update" && !text) {
            if (thinking) updateThinking();
            break;
          }
          const streaming = ev.type !== "message_end";
          setItems((prev) => {
            const last = prev[prev.length - 1];
            if (last?.role === "assistant" && ev.type !== "message_start") {
              return [
                ...prev.slice(0, -1),
                { ...last, text, thinking: false, streaming },
              ];
            }
            return [...prev, { role: "assistant", text, thinking, streaming }];
          });
          break;
        }
        case "tool_execution_start":
          // 记录开始时间，end 时算出 durationMs 展示在卡片上（纯本地计时）
          toolStartTimes.set(ev.toolCallId, Date.now());
          // 清掉滞留 thinking 空泡（模型思考完直接调工具的场景）
          setItems((prev) => {
            const last = prev[prev.length - 1];
            const cleaned =
              last?.role === "assistant" && last.thinking && !last.text
                ? prev.slice(0, -1)
                : prev;
            return [
              ...cleaned,
              {
                role: "tool",
                text: "",
                toolCallId: ev.toolCallId,
                toolName: ev.toolName,
                argsText: JSON.stringify(ev.args ?? {}),
                path:
                  typeof ev.args?.path === "string" ? ev.args.path : undefined,
                pending: true,
              },
            ];
          });
          break;
        case "tool_execution_end": {
          const out =
            (ev.result as ToolResultPayload | null | undefined)?.content
              ?.filter((c) => c.type === "text")
              .map((c) => c.text)
              .join("") ?? "";
          const isError = Boolean(ev.isError);
          const startedAt = toolStartTimes.get(ev.toolCallId);
          toolStartTimes.delete(ev.toolCallId);
          updateItem(ev.toolCallId, {
            isError,
            pending: false,
            expanded: isError ? true : undefined,
            text: `↳ ${String(out).slice(0, 400)}`,
            durationMs:
              typeof startedAt === "number"
                ? Date.now() - startedAt
                : undefined,
          });
          const it = items().find((x) => x.toolCallId === ev.toolCallId);
          if (it?.path) {
            invoke("workspace_backup_info", { path: it.path }).then((info) => {
              if (info !== "null")
                updateItem(ev.toolCallId, { canRevert: true });
            });
          }
          break;
        }
        default:
          break;
      }
    });
    onCleanup(un);

    try {
      await invoke("agent_init");
      await loadHistory();
      // 持久目标（pi-goal）：boot 恢复横幅
      const g = JSON.parse(await invoke<string>("goal_get"));
      setGoal(typeof g === "string" ? g : null);
    } catch (e) {
      push({ role: "status", text: `agent_init failed: ${e}` });
    }
  });

  // message_update 只有 thinking 块时：把当前 assistant 气泡置为思考态
  function updateThinking() {
    setItems((prev) => {
      const last = prev[prev.length - 1];
      if (last?.role === "assistant" && !last.text) {
        return [...prev.slice(0, -1), { ...last, thinking: true }];
      }
      if (last?.role === "assistant") return prev;
      return [...prev, { role: "assistant", text: "", thinking: true }];
    });
  }

  async function sendText(raw: string) {
    const text = raw.trim();
    if (!text || !ready()) return;
    // 命令不依赖 busy：/btw 与主任务并行（pi-btw 并行旁问语义），/plan 亦可
    // 随时起草。
    if (text.startsWith("/")) {
      setInput("");
      if (textareaEl) textareaEl.style.height = "auto";
      await handleCommand(text);
      return;
    }
    // 普通 prompt 对齐 pi TUI 队列语义：响应中发送 = 入队（steering/
    // followUp），当前回合结束后自动继续处理，不丢弃。
    setInput("");
    if (textareaEl) textareaEl.style.height = "auto";
    setStick(true);
    setGoalAuto(null); // 用户手动交互重置 autoContinue 预算（bundle 侧同步重置）
    push({ role: "user", text });
    try {
      await invoke("agent_prompt", { text });
      if (busy())
        push({
          role: "status",
          text: "queued — runs after the current response",
        });
    } catch (e) {
      push({ role: "status", text: `prompt failed: ${e}` });
    }
  }

  // ── 命令类插件（/plan /btw /goal）──
  async function handleCommand(text: string) {
    const [cmd, ...rest] = text.split(/\s+/);
    const arg = rest.join(" ").trim();
    if (cmd === "/plan") {
      if (!arg || planning()) {
        push({ role: "status", text: "usage: /plan <objective>" });
        return;
      }
      setPlanning(true);
      // kick + 事件回投：plan_drafted 事件到达后弹出计划卡
      await invoke("pi_call_global", { fnName: "__pi_plan_start", arg });
      return;
    }
    if (cmd === "/btw") {
      if (!arg) {
        push({ role: "status", text: "usage: /btw <question>" });
        return;
      }
      // 并行旁问：与主任务同时跑，答案经 btw_answer 事件回来
      await invoke("pi_call_global", { fnName: "__pi_btw_start", arg });
      return;
    }
    if (cmd === "/goal") {
      try {
        if (arg === "off" || arg === "clear") {
          await invoke("goal_clear");
          await invoke("pi_call_global", {
            fnName: "__pi_goal_apply",
            arg: "",
          });
          setGoal(null);
          push({ role: "status", text: "goal cleared" });
        } else if (arg) {
          await invoke("goal_set", { objective: arg });
          await invoke("pi_call_global", {
            fnName: "__pi_goal_apply",
            arg: "",
          });
          setGoal(arg);
          push({ role: "status", text: `goal set: ${arg}` });
        } else {
          push({
            role: "status",
            text: "usage: /goal <objective> | /goal off",
          });
        }
      } catch (e) {
        push({ role: "status", text: `goal failed: ${e}` });
      }
      return;
    }
    if (cmd === "/todos") {
      // 面板由 todo_updated 事件驱动；/todos 手动开关（上游为 TUI overlay）
      setTodoOpen(!todoOpen());
      return;
    }
    // 未知命令：匹配技能自定义指令 → 透传（bundle 按 skill 展开）；否则提示
    if (skillCmds().some((c) => c.cmd === cmd)) {
      try {
        await invoke("agent_prompt", { text });
      } catch (e) {
        push({ role: "status", text: `command failed: ${e}` });
      }
      return;
    }
    push({
      role: "status",
      text: `unknown command ${cmd} — try ${allCommands()
        .map((c) => c.cmd)
        .join(", ")}`,
    });
  }

  async function continueGoal() {
    const g = goal();
    if (!g || busy()) return;
    setStick(true);
    push({ role: "user", text: "▶ continue the current goal" });
    try {
      await invoke("agent_prompt", {
        text: `Continue working toward the current goal: ${g}. Pick up where you left off.`,
      });
    } catch (e) {
      push({ role: "status", text: `prompt failed: ${e}` });
    }
  }

  async function clearGoal() {
    try {
      await invoke("goal_clear");
      await invoke("pi_call_global", { fnName: "__pi_goal_apply", arg: "" });
      setGoal(null);
      setGoalAuto(null);
      push({ role: "status", text: "goal cleared" });
    } catch (e) {
      push({ role: "status", text: `goal clear failed: ${e}` });
    }
  }

  async function stop() {
    try {
      await invoke("agent_stop");
    } catch (e) {
      push({ role: "status", text: `stop failed: ${e}` });
    }
  }

  async function decide(decision: "allow" | "deny" | "always") {
    const a = approval();
    if (!a) return;
    setApproval(null);
    try {
      await invoke("approval_respond", { requestId: a.requestId, decision });
      push({ role: "status", text: `${a.tool} ${a.path} → ${decision}` });
    } catch (e) {
      push({ role: "status", text: `approval respond failed: ${e}` });
    }
  }

  // ── ask_user（pi-ask-user 移动原生化）──
  const toggleOption = (title: string) => {
    const a = ask();
    if (!a) return;
    if (a.allowMultiple) {
      setAsk({
        ...a,
        selected: a.selected.includes(title)
          ? a.selected.filter((t) => t !== title)
          : [...a.selected, title],
      });
    } else {
      setAsk({ ...a, selected: [title], freeform: "" });
    }
  };

  const setFreeform = (v: string) => {
    const a = ask();
    if (!a) return;
    setAsk({ ...a, freeform: v, selected: v.trim() ? [] : a.selected });
  };

  const setAskComment = (v: string) => {
    const a = ask();
    if (!a) return;
    setAsk({ ...a, comment: v });
  };

  async function answerAsk(cancelled: boolean) {
    const a = ask();
    if (!a) return;
    let response: unknown = null;
    if (!cancelled) {
      if (a.freeform.trim()) {
        response = {
          kind: "freeform",
          text: a.freeform.trim(),
          comment: a.comment.trim() || undefined,
        };
      } else if (a.selected.length) {
        response = {
          kind: "selection",
          selections: a.selected,
          comment: a.comment.trim() || undefined,
        };
      } else {
        return; // 没有任何回答，不让空提交
      }
    }
    setAsk(null);
    try {
      await invoke("ask_user_respond", {
        requestId: a.requestId,
        answer: JSON.stringify({ response, cancelled }),
      });
      push({
        role: "status",
        text: cancelled ? "question dismissed" : "question answered",
      });
    } catch (e) {
      push({ role: "status", text: `ask_user respond failed: ${e}` });
    }
  }

  async function openDrawer() {
    setFilesOpen(false);
    setDrawerOpen(true);
    try {
      await sess.refreshList();
      await mcp.refreshList();
      await skills.refreshList();
    } catch (e) {
      push({ role: "status", text: `session_list failed: ${e}` });
    }
  }

  async function openFiles() {
    setDrawerOpen(false);
    setFilesOpen(true);
    try {
      setTree(JSON.parse(await invoke<string>("workspace_tree")));
    } catch (e) {
      push({ role: "status", text: `workspace_tree failed: ${e}` });
    }
  }

  async function previewFile(path: string) {
    try {
      const content = await invoke<string>("workspace_read", { path });
      setPreview({ path, content });
    } catch (e) {
      push({ role: "status", text: `read failed: ${e}` });
    }
  }

  // 顶栏标题：当前会话的标题（与侧栏同源：取最后一条消息文本）。
  //
  // 有意**不**退化到会话 id —— 顶栏原来显示的是 id 短码，对用户没有意义，
  // 已按要求移除；没有消息时给「New chat」而不是一串乱码。
  const currentSessionTitle = createMemo(() => {
    const id = sess.currentSession();
    if (!id) return "pi-mobile";
    const s = sess.sessions().find((x) => x.id === id);
    return (s?.lastMessage ?? "").trim() || "New chat";
  });

  // ── 组合回调（跨子系统的 UI 编排，原 JSX 内联逻辑原样收拢）──

  const onPickCommand = (cmd: string) => {
    setInput(`${cmd} `);
    textareaEl?.focus();
  };

  const onOpenSettings = () => {
    setSettingsView("providers");
    void prov.refreshConfigured();
    setDrawerOpen(false);
    setSettingsOpen(true);
  };

  const onOpenAgentTab = () => {
    setSettingsView("agent");
    invoke<string>("approval_policy_get")
      .then((p) => setApprovalPolicy(p === "auto" ? "auto" : "ask"))
      .catch(() => {});
    // 设备能力清单（含权限态）与审批策略同页，一起拉
    void refreshNativeCaps();
  };

  const onOAuthLogin = () => {
    void invoke("pi_call_global", {
      fnName: "__pi_oauth_login",
      arg: prov.selProvider(),
    }).catch((e) => push({ role: "status", text: `oauth login failed: ${e}` }));
    push({
      role: "status",
      text: `signing in with ${prov.providerLabel(prov.selProvider())}…`,
    });
  };

  const onOpenProvider = (id: string) => {
    void prov.chooseProvider(id);
    setSettingsView("provider");
  };

  const onChangePickerProvider = () => {
    prov.setModelPickerOpen(false);
    setSettingsView("providers");
    void prov.refreshConfigured();
    setSettingsOpen(true);
  };

  const onApprovePlan = () => {
    const planData = plan();
    setPlan(null);
    if (planData)
      sendText(`✅ Plan approved — execute it now:\n\n${planData.content}`);
  };

  return (
    <main class="app">
      <TopBar
        title={currentSessionTitle}
        tokens={sessionTokens}
        onOpenDrawer={openDrawer}
      />

      <GoalBanner
        goal={goal}
        goalAuto={goalAuto}
        busy={busy}
        onContinue={continueGoal}
        onClear={clearGoal}
      />

      <ProviderSetup
        show={() => !ready() || !prov.currentModel()}
        providers={prov.providers}
        selProvider={prov.selProvider}
        chooseProvider={prov.chooseProvider}
        keySaved={prov.keySaved}
        setKeySaved={prov.setKeySaved}
        providerKey={prov.providerKey}
        setProviderKey={prov.setProviderKey}
        saveProviderKey={prov.saveProviderKey}
        loadingModels={prov.loadingModels}
        providerModels={prov.providerModels}
        providerLabel={prov.providerLabel}
        selectModel={prov.selectModel}
      />

      <ChatStream
        items={items}
        ready={ready}
        stick={stick}
        setStick={setStick}
        reducedMotion={reducedMotion}
        sendText={sendText}
        updateItem={updateItem}
        revert={revert}
      />

      <ApprovalCard approval={approval} capLabel={capLabel} onDecide={decide} />

      <AskUserCard
        ask={ask}
        onToggleOption={toggleOption}
        onSetFreeform={setFreeform}
        onSetComment={setAskComment}
        onAnswer={answerAsk}
      />

      <CommandPalette
        visible={() => input().startsWith("/")}
        commands={allCommands}
        onPick={onPickCommand}
      />

      <PlanCard
        plan={plan}
        planning={planning}
        onDiscard={() => setPlan(null)}
        onApproveRun={onApprovePlan}
      />

      <TodoPanel
        todoOpen={todoOpen}
        todos={todos}
        onClose={() => setTodoOpen(false)}
      />

      {/* composer 区：信任行（small-caps 微文案）+ 卡片化容器（chips 内嵌上方、
          输入行下方，参考 agent-workflow-ios 设计稿的层次） */}
      <div class="trust-row">
        <span>ON-DEVICE · SANDBOXED</span>
      </div>
      <div class="composer-card">
        <QuickBar
          ready={ready}
          onOpenFiles={openFiles}
          onOpenModelPicker={prov.openModelPicker}
          modelName={() => prov.currentModel()?.name ?? "Model"}
          todoOpen={todoOpen}
          onToggleTodos={() => setTodoOpen(!todoOpen())}
        />
        <Composer
          input={input}
          setInput={setInput}
          ready={ready}
          busy={busy}
          onSend={sendText}
          onStop={stop}
          registerTextarea={(el) => {
            textareaEl = el;
          }}
        />
      </div>

      <SessionDrawer
        open={drawerOpen}
        onOpenChange={setDrawerOpen}
        onNewSession={sess.newSession}
        sessionSearch={sess.sessionSearch}
        setSessionSearch={sess.setSessionSearch}
        sessionGroups={sess.sessionGroups}
        currentSession={sess.currentSession}
        onSwitchSession={sess.switchSession}
        onDeleteSession={sess.deleteSession}
        onOpenPreview={() => {
          setDrawerOpen(false);
          void previewState.openPreview();
        }}
        onOpenSettings={onOpenSettings}
      />

      <SettingsSheet
        open={settingsOpen}
        onOpenChange={setSettingsOpen}
        view={settingsView}
        setView={setSettingsView}
        onOpenAgentTab={onOpenAgentTab}
        busy={busy}
        providers={prov.providers}
        currentModel={prov.currentModel}
        configured={prov.configured}
        onOpenProvider={onOpenProvider}
        selProvider={prov.selProvider}
        providerLabel={prov.providerLabel}
        providerKey={prov.providerKey}
        setProviderKey={prov.setProviderKey}
        saveProviderKey={prov.saveProviderKey}
        keySaved={prov.keySaved}
        setKeySaved={prov.setKeySaved}
        loadingModels={prov.loadingModels}
        loadModels={prov.loadModels}
        providerModels={prov.providerModels}
        selectModel={prov.selectModel}
        onOAuthLogin={onOAuthLogin}
        mcpServers={mcp.mcpServers}
        mcpReady={mcp.mcpReady}
        onReconnectMcp={mcp.reconnectMcp}
        onRemoveMcp={mcp.removeMcpServer}
        mcpName={mcp.mcpName}
        setMcpName={mcp.setMcpName}
        mcpUrl={mcp.mcpUrl}
        setMcpUrl={mcp.setMcpUrl}
        mcpTimeout={mcp.mcpTimeout}
        setMcpTimeout={mcp.setMcpTimeout}
        mcpHeaders={mcp.mcpHeaders}
        setMcpHeaders={mcp.setMcpHeaders}
        addMcpServer={mcp.addMcpServer}
        mcpPasteOpen={mcp.mcpPasteOpen}
        setMcpPasteOpen={mcp.setMcpPasteOpen}
        mcpPaste={mcp.mcpPaste}
        setMcpPaste={mcp.setMcpPaste}
        importMcpJson={mcp.importMcpJson}
        skills={skills.skills}
        onRemoveSkill={skills.removeSkill}
        onToggleSkill={skills.toggleSkill}
        skillUrl={skills.skillUrl}
        setSkillUrl={skills.setSkillUrl}
        installingSkill={skills.installingSkill}
        installSkill={skills.installSkill}
        approvalPolicy={approvalPolicy}
        onSetApprovalPolicy={(next) => void setApprovalPolicyPersist(next)}
        nativeCaps={nativeCaps}
        nativeErr={nativeErr}
        onRequestPermission={requestNativePermission}
      />

      <ModelPicker
        open={prov.modelPickerOpen}
        onOpenChange={prov.setModelPickerOpen}
        pickerProvider={prov.pickerProvider}
        providerLabel={prov.providerLabel}
        pickerModels={prov.pickerModels}
        currentModel={prov.currentModel}
        selectModel={prov.selectModel}
        onChangeProvider={onChangePickerProvider}
      />

      <WorkspaceDrawer
        open={filesOpen}
        onOpenChange={setFilesOpen}
        onRefresh={openFiles}
        tree={tree}
        onOpenFile={previewFile}
      />

      <FilePreviewDialog
        preview={preview}
        tree={tree}
        onClose={() => setPreview(null)}
      />

      <PreviewPanel
        open={previewState.previewOpen}
        previewPath={previewState.previewPath}
        previewList={previewState.previewList}
        previewSrc={previewState.previewSrc}
        previewErr={previewState.previewErr}
        onPickPath={(path) => {
          previewState.setPreviewPath(path);
          previewState.setPreviewNonce((n) => n + 1);
        }}
        onReload={() => previewState.setPreviewNonce((n) => n + 1)}
        onOpenExternal={() => {
          const p = previewState.previewPath();
          const port = previewState.previewPort();
          if (p && port)
            void invoke("preview_open_external", { port, path: p }).catch((e) =>
              previewState.setPreviewErr(`open in browser: ${e}`),
            );
        }}
        onClose={() => previewState.setPreviewOpen(false)}
      />
    </main>
  );
}

export default App;
