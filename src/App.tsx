import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  For,
  Show,
  createEffect,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import { Markdown } from "./ui/Markdown";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "~/components/ui/collapsible";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from "~/components/ui/sheet";
import { TextField, TextFieldInput } from "~/components/ui/text-field";
import "./App.css";

type ChatItem = {
  role: "user" | "assistant" | "tool" | "status";
  text: string;
  toolCallId?: string;
  toolName?: string;
  argsText?: string;
  pending?: boolean;
  isError?: boolean;
  expanded?: boolean;
  thinking?: boolean;
  path?: string;
  canRevert?: boolean;
  reverted?: boolean;
};

type SessionMeta = {
  id: string;
  createdAt: number;
  modifiedAt: number;
  cwd: string;
  entries: number;
  size: number;
};

type Approval = {
  requestId: string;
  tool: string;
  path: string;
  diff: string;
};

type AskOption = { title: string; description?: string };

type AskRequest = {
  requestId: string;
  question: string;
  context?: string | null;
  options: AskOption[];
  allowMultiple: boolean;
  allowFreeform: boolean;
  allowComment: boolean;
  selected: string[];
  freeform: string;
  comment: string;
};

type TreeEntry = {
  path: string;
  kind: "file" | "directory";
  size: number;
  mtimeMs: number;
};

type TodoTask = {
  id: number;
  subject: string;
  description?: string;
  activeForm?: string;
  status: "pending" | "in_progress" | "completed" | "deleted";
  blockedBy?: number[];
  owner?: string;
};

type SkillMeta = {
  id: string;
  name: string;
  description: string;
  source: string;
  version: string;
  checksum: string;
  enabled: boolean;
  installedAt: number;
};

const SUGGESTIONS = [
  "List my workspace files",
  "Create hello.py that prints a greeting",
  "What can you do?",
];

const COMMANDS = [
  { cmd: "/plan", desc: "draft an implementation plan" },
  { cmd: "/btw", desc: "quick side question (context-aware)" },
  { cmd: "/goal", desc: "set a persistent objective (/goal off clears)" },
  { cmd: "/todos", desc: "show/hide the agent's task list panel" },
];

function fmtRel(ms: number): string {
  const mins = Math.floor((Date.now() - ms) / 60000);
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

function App() {
  const [items, setItems] = createSignal<ChatItem[]>([
    { role: "status", text: "booting embedded pi agent…" },
  ]);
  const [input, setInput] = createSignal("");
  const [apiKey, setApiKey] = createSignal("");
  const [ready, setReady] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [approval, setApproval] = createSignal<Approval | null>(null);
  const [ask, setAsk] = createSignal<AskRequest | null>(null);
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [sessions, setSessions] = createSignal<SessionMeta[]>([]);
  const [mcpServers, setMcpServers] = createSignal<{ name: string; url: string; timeoutMs?: number; headers?: Record<string, string> }[]>([]);
  const [mcpName, setMcpName] = createSignal("");
  const [mcpUrl, setMcpUrl] = createSignal("");
  const [mcpTimeout, setMcpTimeout] = createSignal("");
  const [mcpHeaders, setMcpHeaders] = createSignal("");
  const [skills, setSkills] = createSignal<SkillMeta[]>([]);
  const [skillUrl, setSkillUrl] = createSignal("");
  const [installingSkill, setInstallingSkill] = createSignal(false);
  const [currentSession, setCurrentSession] = createSignal<string | null>(null);
  const [filesOpen, setFilesOpen] = createSignal(false);
  const [tree, setTree] = createSignal<TreeEntry[]>([]);
  const [preview, setPreview] = createSignal<{ path: string; content: string } | null>(
    null,
  );
  const [stick, setStick] = createSignal(true);
  const [plan, setPlan] = createSignal<{ objective: string; content: string } | null>(
    null,
  );
  const [planning, setPlanning] = createSignal(false);
  const [goal, setGoal] = createSignal<string | null>(null);
  const [todos, setTodos] = createSignal<{ tasks: TodoTask[]; nextId: number }>({
    tasks: [],
    nextId: 1,
  });
  const [todoOpen, setTodoOpen] = createSignal(false);

  let chatEl: HTMLDivElement | undefined;
  let textareaEl: HTMLTextAreaElement | undefined;

  const push = (item: ChatItem) => setItems((prev) => [...prev, item]);
  const updateItem = (toolCallId: string, patch: Partial<ChatItem>) =>
    setItems((prev) =>
      prev.map((it) =>
        it.toolCallId === toolCallId ? { ...it, ...patch } : it,
      ),
    );

  const hasConversation = () =>
    items().some((i) => i.role === "user" || i.role === "assistant" || i.role === "tool");

  // ── 自动滚动：贴底跟随，用户上翻即暂停 ──
  createEffect(() => {
    items();
    if (chatEl && stick()) {
      chatEl.scrollTop = chatEl.scrollHeight;
    }
  });
  const onChatScroll = () => {
    if (!chatEl) return;
    setStick(chatEl.scrollHeight - chatEl.scrollTop - chatEl.clientHeight < 90);
  };

  function mapHistoryMessage(m: any): ChatItem {
    const text =
      typeof m.content === "string"
        ? m.content
        : (m.content ?? [])
            .filter((c: any) => c.type === "text")
            .map((c: any) => c.text)
            .join("");
    if (m.role === "toolResult") {
      return { role: "tool", text: `↳ ${text.slice(0, 200)}` };
    }
    if (m.role === "assistant") {
      const calls = (m.content ?? [])
        .filter((c: any) => c.type === "toolCall")
        .map((c: any) => `⚒ ${c.name}(${JSON.stringify(c.arguments ?? {})})`);
      return { role: "assistant", text: text || calls.join("\n") };
    }
    return { role: "user", text };
  }

  async function loadHistory() {
    const h = JSON.parse(await invoke<string>("agent_history"));
    setCurrentSession(h.sessionId ?? null);
    const msgs = (h.messages ?? []) as any[];
    if (!msgs.length) {
      setItems([]);
      return;
    }
    setItems(msgs.map(mapHistoryMessage));
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
      push({ role: "status", text: `reverted ${it.path} to the previous version` });
    } catch (e) {
      push({ role: "status", text: `revert failed: ${e}` });
    }
  }

  onMount(async () => {
    const un = await listen<string>("pi-agent-event", (e) => {
      let ev: any;
      try {
        ev = JSON.parse(e.payload);
      } catch {
        return;
      }
      switch (ev.type) {
        case "agent_ready":
          setReady(true);
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
          setCurrentSession(ev.sessionId ?? null);
          break;
        case "session_created":
          setCurrentSession(ev.sessionId ?? null);
          break;
        case "session_error":
          push({ role: "status", text: `session persist error: ${ev.error}` });
          break;
        case "approval_required":
          setApproval({
            requestId: ev.requestId,
            tool: ev.tool,
            path: ev.path,
            diff: ev.diff ?? "",
          });
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
        case "mcp_ready":
          push({ role: "status", text: `mcp ${ev.server} ready — ${(ev.tools ?? []).length} tools` });
          break;
        case "mcp_error":
          push({ role: "status", text: `mcp ${ev.server}: ${ev.error}` });
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
          push({ role: "assistant", text: `💬 ${ev.question}\n\n${ev.answer ?? ""}` });
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
        case "boot_error":
          setBusy(false);
          push({ role: "status", text: `BOOT ERROR: ${ev.error}` });
          break;
        case "turn_end":
          // 回合收束：清掉滞留的 thinking 空泡
          setItems((prev) => {
            const last = prev[prev.length - 1];
            return last?.role === "assistant" && last.thinking && !last.text
              ? prev.slice(0, -1)
              : prev;
          });
          break;
        case "message_start":
        case "message_update":
        case "message_end": {
          // 只渲染 assistant 流（user/toolResult 的消息事件另行处理/已在界面）
          const msg = ev.message ?? {};
          if (msg.role !== "assistant") break;
          const blocks = msg.content ?? [];
          const text = blocks
            .filter((c: any) => c.type === "text")
            .map((c: any) => c.text)
            .join("");
          const thinking = blocks.some((c: any) => c.type === "thinking");
          if (ev.type === "message_update" && !text) {
            if (thinking) updateThinking();
            break;
          }
          setItems((prev) => {
            const last = prev[prev.length - 1];
            if (last?.role === "assistant" && ev.type !== "message_start") {
              return [...prev.slice(0, -1), { ...last, text, thinking: false }];
            }
            return [...prev, { role: "assistant", text, thinking }];
          });
          break;
        }
        case "tool_execution_start":
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
                path: ev.args?.path,
                pending: true,
              },
            ];
          });
          break;
        case "tool_execution_end": {
          const out =
            ev.result?.content
              ?.filter((c: any) => c.type === "text")
              .map((c: any) => c.text)
              .join("") ?? "";
          const isError = Boolean(ev.isError);
          updateItem(ev.toolCallId, {
            isError,
            pending: false,
            expanded: isError ? true : undefined,
            text: `↳ ${String(out).slice(0, 400)}`,
          });
          const it = items().find((x) => x.toolCallId === ev.toolCallId);
          if (it?.path) {
            invoke("workspace_backup_info", { path: it.path }).then((info) => {
              if (info !== "null") updateItem(ev.toolCallId, { canRevert: true });
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

  async function saveKey(e: Event) {
    e.preventDefault();
    if (!apiKey().trim()) return;
    await invoke("set_creds", {
      provider: "deepseek",
      apiKey: apiKey().trim(),
    });
    setApiKey("");
    push({ role: "status", text: "API key saved (deepseek)" });
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
    push({ role: "user", text });
    try {
      await invoke("agent_prompt", { text });
      if (busy()) push({ role: "status", text: "queued — runs after the current response" });
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
          await invoke("pi_call_global", { fnName: "__pi_goal_apply", arg: "" });
          setGoal(null);
          push({ role: "status", text: "goal cleared" });
        } else if (arg) {
          await invoke("goal_set", { objective: arg });
          await invoke("pi_call_global", { fnName: "__pi_goal_apply", arg: "" });
          setGoal(arg);
          push({ role: "status", text: `goal set: ${arg}` });
        } else {
          push({ role: "status", text: "usage: /goal <objective> | /goal off" });
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
    push({ role: "status", text: `unknown command ${cmd} — try ${COMMANDS.map((c) => c.cmd).join(", ")}` });
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
      push({ role: "status", text: "goal cleared" });
    } catch (e) {
      push({ role: "status", text: `goal clear failed: ${e}` });
    }
  }

  // ── todo 面板（@juicesharp/rpiv-todo 移动原生化）──
  // 上游 TUI overlay 的移动形态：列表非空自动显示（todo_updated 事件驱动），
  // ✓ 完成 / ◐ 进行中（带 activeForm）/ ○ 待办；墓碑行不上屏。
  const visibleTodos = () => todos().tasks.filter((t) => t.status !== "deleted");
  const todoHeading = () => {
    const all = visibleTodos();
    const done = all.filter((t) => t.status === "completed").length;
    return `Todos (${done}/${all.length})`;
  };
  const todoGlyph = (t: TodoTask) =>
    t.status === "completed" ? "✓" : t.status === "in_progress" ? "◐" : "○";

  const onSubmit = (e: Event) => {
    e.preventDefault();
    sendText(input());
  };

  const onKeydown = (e: KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey && !(e as any).isComposing) {
      e.preventDefault();
      sendText(input());
    }
  };

  const autoGrow = () => {
    if (!textareaEl) return;
    textareaEl.style.height = "auto";
    textareaEl.style.height = `${Math.min(textareaEl.scrollHeight, 132)}px`;
  };

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
      setSessions(JSON.parse(await invoke<string>("session_list")));
      setMcpServers(JSON.parse(await invoke<string>("mcp_list")));
      setSkills(JSON.parse(await invoke<string>("skills_list")));
    } catch (e) {
      push({ role: "status", text: `session_list failed: ${e}` });
    }
  }

  // ── Skills（D12）：URL 安装 / 启停 / 删除，改完经 skills_reconnect 热注入 ──
  async function refreshSkills() {
    try {
      setSkills(JSON.parse(await invoke<string>("skills_list")));
    } catch (e) {
      push({ role: "status", text: `skills_list failed: ${e}` });
    }
  }

  async function installSkill(e: Event) {
    e.preventDefault();
    const url = skillUrl().trim();
    if (!url || installingSkill()) return;
    setInstallingSkill(true);
    try {
      const entry = JSON.parse(await invoke<string>("skills_install", { url }));
      await invoke("skills_reconnect");
      await refreshSkills();
      setSkillUrl("");
      push({ role: "status", text: `skill installed: ${entry.id} (${entry.version})` });
    } catch (err) {
      push({ role: "status", text: `skills_install failed: ${err}` });
    } finally {
      setInstallingSkill(false);
    }
  }

  async function toggleSkill(id: string, enabled: boolean) {
    try {
      await invoke("skills_toggle", { id, enabled });
      await invoke("skills_reconnect");
      await refreshSkills();
    } catch (e) {
      push({ role: "status", text: `skills_toggle failed: ${e}` });
    }
  }

  async function removeSkill(id: string) {
    try {
      await invoke("skills_remove", { id });
      await invoke("skills_reconnect");
      await refreshSkills();
      push({ role: "status", text: `skill removed: ${id}` });
    } catch (e) {
      push({ role: "status", text: `skills_remove failed: ${e}` });
    }
  }

  async function addMcpServer(e: Event) {
    e.preventDefault();
    if (!mcpName().trim() || !mcpUrl().trim()) return;
    // 请求头：每行 "Key: Value"，解析为 JSON 对象
    const headers: Record<string, string> = {};
    for (const line of mcpHeaders().split("\n")) {
      const idx = line.indexOf(":");
      if (idx === -1) continue;
      const key = line.slice(0, idx).trim();
      const val = line.slice(idx + 1).trim();
      if (key && val) headers[key] = val;
    }
    const timeoutMs = Number(mcpTimeout()) || 30_000;
    try {
      await invoke("mcp_add", {
        name: mcpName().trim(),
        url: mcpUrl().trim(),
        timeoutMs,
        headers,
      });
      setMcpServers(JSON.parse(await invoke<string>("mcp_list")));
      setMcpName("");
      setMcpUrl("");
      setMcpTimeout("");
      setMcpHeaders("");
      push({ role: "status", text: `mcp server saved — reconnecting…` });
      await invoke("mcp_reconnect");
    } catch (e) {
      push({ role: "status", text: `mcp_add failed: ${e}` });
    }
  }

  async function reconnectMcp() {
    push({ role: "status", text: "reconnecting mcp servers…" });
    try {
      await invoke("mcp_reconnect");
    } catch (e) {
      push({ role: "status", text: `mcp_reconnect failed: ${e}` });
    }
  }

  async function removeMcpServer(name: string) {
    try {
      await invoke("mcp_remove", { name });
      setMcpServers(JSON.parse(await invoke<string>("mcp_list")));
    } catch (e) {
      push({ role: "status", text: `mcp_remove failed: ${e}` });
    }
  }

  async function switchSession(id: string) {
    setDrawerOpen(false);
    try {
      await invoke("session_open", { id });
      await loadHistory();
    } catch (e) {
      push({ role: "status", text: `session switch failed: ${e}` });
    }
  }

  async function newSession() {
    setDrawerOpen(false);
    try {
      await invoke("session_new");
      setCurrentSession(null);
      setItems([{ role: "status", text: "new session started" }]);
    } catch (e) {
      push({ role: "status", text: `session_new failed: ${e}` });
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

  const copyText = (text: string) => {
    navigator.clipboard?.writeText(text).catch(() => {});
  };

  const toolState = (it: ChatItem) =>
    it.pending ? "pending" : it.isError ? "error" : "ok";

  const prettyArgs = (raw?: string) => {
    if (!raw) return "";
    try {
      return JSON.stringify(JSON.parse(raw), null, 2);
    } catch {
      return raw;
    }
  };

  return (
    <main class="app">
      <header class="topbar">
        <div class="topbar-actions">
          <Button variant="secondary" size="icon" class="h-8 w-8" onClick={openDrawer} aria-label="sessions">
            ☰
          </Button>
          <Button variant="secondary" size="icon" class="h-8 w-8" onClick={openFiles} aria-label="files">
            📁
          </Button>
        </div>
        <h1 class="topbar-title">pi-mobile</h1>
        <div class="topbar-meta">
          <Show when={currentSession()}>{currentSession()!.slice(0, 8)}</Show>
        </div>
      </header>

      <Show when={goal()}>
        {(g) => (
          <div class="goal-banner">
            <span class="goal-text">🎯 {g()}</span>
            <div class="goal-actions">
              <Show when={!busy()}>
                <button class="goal-btn" onClick={continueGoal}>
                  ▶ Continue
                </button>
              </Show>
              <button class="goal-btn" onClick={clearGoal}>
                ✕
              </button>
            </div>
          </div>
        )}
      </Show>

      <Show when={!ready()}>
        <form class="keyform" onSubmit={saveKey}>
          <TextField class="flex-1">
            <TextFieldInput
              type="password"
              placeholder="DeepSeek API key…"
              value={apiKey()}
              onInput={(e) => setApiKey(e.currentTarget.value)}
            />
          </TextField>
          <Button type="submit">Save</Button>
        </form>
      </Show>

      <div class="chat" ref={chatEl} onScroll={onChatScroll}>
        <Show when={ready() && !hasConversation()}>
          <div class="welcome">
            <div class="welcome-logo">π</div>
            <h2>Your pocket coding agent</h2>
            <p>
              pi runs entirely on this device — it can list, read, write and edit
              files in the sandboxed workspace. Writes ask for your approval.
            </p>
            <div class="chips">
              <For each={SUGGESTIONS}>
                {(s) => (
                  <Button
                    variant="outline"
                    size="sm"
                    class="rounded-full"
                    onClick={() => sendText(s)}
                  >
                    {s}
                  </Button>
                )}
              </For>
            </div>
          </div>
        </Show>

        <For each={items()}>
          {(it) => (
            <Show
              when={it.role !== "tool" || it.toolCallId}
              fallback={
                <div class="result-line">{it.text}</div>
              }
            >
              <div class={`msg msg-${it.role}`}>
                <Show
                  when={it.role === "tool"}
                  fallback={
                    <Show
                      when={it.role === "assistant"}
                      fallback={
                        <Show
                          when={it.role === "user"}
                          fallback={<span class="status-line">{it.text}</span>}
                        >
                          <div class="bubble">{it.text}</div>
                        </Show>
                      }
                    >
                      <div class={`bubble ${it.thinking ? "thinking" : ""}`}>
                        <Show when={!it.thinking} fallback={<span>thinking…</span>}>
                          <Markdown text={it.text} />
                        </Show>
                        <Show when={it.text && !it.thinking}>
                          <button
                            class="copy-btn"
                            onClick={() => copyText(it.text)}
                            aria-label="copy"
                          >
                            copy
                          </button>
                        </Show>
                      </div>
                    </Show>
                  }
                >
                  <Collapsible
                    open={it.expanded}
                    onOpenChange={(o) => updateItem(it.toolCallId!, { expanded: o })}
                    class={`tool-card ${toolState(it)}`}
                  >
                    <CollapsibleTrigger class="tool-head">
                      <Badge
                        variant={
                          it.pending ? "warning" : it.isError ? "destructive" : "success"
                        }
                        class="px-1.5 text-[0.6rem]"
                      >
                        {it.pending ? "…" : it.isError ? "!" : "✓"}
                      </Badge>
                      <span class="tool-summary">
                        {it.toolName}({(it.argsText ?? "").slice(0, 90)})
                        {it.pending ? " …" : ""}
                      </span>
                      <span class="tool-caret">{it.expanded ? "▼" : "▶"}</span>
                    </CollapsibleTrigger>
                    <CollapsibleContent>
                      <div class="tool-body">
                        <div>{prettyArgs(it.argsText)}</div>
                        <Show when={it.text}>
                          <div class="tool-result">{it.text}</div>
                        </Show>
                        <Show when={it.path && (it.canRevert || it.reverted)}>
                          <div class="tool-revert-row">
                            <Show
                              when={it.canRevert}
                              fallback={<span class="reverted-note">↩ reverted</span>}
                            >
                              <Button
                                variant="secondary"
                                size="sm"
                                class="h-7 rounded-full text-xs"
                                onClick={() => revert(it)}
                              >
                                ↩ Revert
                              </Button>
                            </Show>
                          </div>
                        </Show>
                      </div>
                    </CollapsibleContent>
                  </Collapsible>
                </Show>
              </div>
            </Show>
          )}
        </For>
      </div>

      <Show when={approval()}>
        {(a) => (
          <div class="approval">
            <div class="approval-title">
              ⚠ {a().tool} «{a().path}» — approve?
            </div>
            <Show when={a().diff}>
              <div class="diff">
                {a().diff.split("\n").map((line) => (
                  <div
                    class={
                      line.startsWith("+")
                        ? "diff-add"
                        : line.startsWith("-")
                          ? "diff-del"
                          : "diff-ctx"
                    }
                  >
                    {line || " "}
                  </div>
                ))}
              </div>
            </Show>
            <div class="approval-actions">
              <Button variant="destructive" onClick={() => decide("deny")}>
                Deny
              </Button>
              <Button variant="secondary" onClick={() => decide("always")}>
                Always
              </Button>
              <Button
                class="bg-success text-success-foreground hover:bg-success/90"
                onClick={() => decide("allow")}
              >
                Allow
              </Button>
            </div>
          </div>
        )}
      </Show>

      <Show when={ask()}>
        {(a) => (
          <div class="approval">
            <div class="approval-title">❓ {a().question}</div>
            <Show when={a().context}>
              <div class="ask-context">{a().context}</div>
            </Show>
            <Show when={a().options.length}>
              <div class="ask-options">
                <For each={a().options}>
                  {(o) => (
                    <button
                      class={`ask-option ${a().selected.includes(o.title) ? "selected" : ""}`}
                      onClick={() => toggleOption(o.title)}
                    >
                      <div class="ask-option-title">
                        <span class="ask-option-mark">
                          {a().selected.includes(o.title) ? "●" : "○"}
                        </span>
                        {o.title}
                      </div>
                      <Show when={o.description}>
                        <div class="ask-option-desc">{o.description}</div>
                      </Show>
                    </button>
                  )}
                </For>
              </div>
            </Show>
            <Show when={a().allowFreeform}>
              <input
                class="ask-input"
                placeholder="Or write your own answer…"
                value={a().freeform}
                onInput={(e) => setFreeform(e.currentTarget.value)}
              />
            </Show>
            <Show when={a().allowComment}>
              <input
                class="ask-input"
                placeholder="Optional comment…"
                value={a().comment}
                onInput={(e) => setAsk({ ...a(), comment: e.currentTarget.value })}
              />
            </Show>
            <div class="approval-actions">
              <Button variant="secondary" onClick={() => answerAsk(true)}>
                Skip
              </Button>
              <Button
                onClick={() => answerAsk(false)}
                disabled={!a().selected.length && !a().freeform.trim()}
              >
                Answer
              </Button>
            </div>
          </div>
        )}
      </Show>

      <Show when={input().startsWith("/")}>
        <div class="cmd-palette">
          <For each={COMMANDS}>
            {(c) => (
              <button
                class="cmd-row"
                onClick={() => {
                  setInput(`${c.cmd} `);
                  textareaEl?.focus();
                }}
              >
                <span class="cmd-name">{c.cmd}</span>
                <span class="cmd-desc">{c.desc}</span>
              </button>
            )}
          </For>
        </div>
      </Show>

      <Show when={plan()}>
        {(p) => (
          <div class="approval">
            <div class="approval-title">📋 Plan — {p().objective}</div>
            <div class="md plan-body">
              <Markdown text={p().content} />
            </div>
            <div class="approval-actions">
              <Button variant="secondary" onClick={() => setPlan(null)}>
                Discard
              </Button>
              <Button
                class="bg-success text-success-foreground hover:bg-success/90"
                onClick={() => {
                  const planData = plan();
                  setPlan(null);
                  if (planData) sendText(`✅ Plan approved — execute it now:\n\n${planData.content}`);
                }}
              >
                ▶ Approve & run
              </Button>
            </div>
          </div>
        )}
      </Show>

      <Show when={planning()}>
        <div class="planning-note">drafting plan…</div>
      </Show>

      <Show when={todoOpen()}>
        <div class="todo-panel">
          <div class="todo-head">
            <span class="todo-title">{todoHeading()}</span>
            <button class="goal-btn" onClick={() => setTodoOpen(false)} aria-label="hide todos">
              ✕
            </button>
          </div>
          <Show
            when={visibleTodos().length > 0}
            fallback={<div class="todo-row todo-empty">No todos yet. Ask the agent to add some!</div>}
          >
            <For each={visibleTodos()}>
              {(t) => (
                <div class={`todo-row todo-${t.status}`}>
                  <span class="todo-glyph">{todoGlyph(t)}</span>
                  <span class="todo-subject">
                    #{t.id} {t.subject}
                    <Show when={t.status === "in_progress" && t.activeForm}>
                      <span class="todo-active"> ({t.activeForm})</span>
                    </Show>
                  </span>
                </div>
              )}
            </For>
          </Show>
        </div>
      </Show>

      <form class="composer" onSubmit={onSubmit}>
        <textarea
          ref={textareaEl}
          rows="1"
          placeholder={
            ready() ? (busy() ? "Queue a message while pi works…" : "Ask pi to do something…") : "agent booting…"
          }
          disabled={!ready()}
          value={input()}
          onInput={(e) => {
            setInput(e.currentTarget.value);
            autoGrow();
          }}
          onKeyDown={onKeydown}
        />
        <Show
          when={busy() && !input().trim()}
          fallback={
            <button
              type="submit"
              class="send-btn"
              disabled={!ready() || !input().trim()}
              aria-label="send"
            >
              ➤
            </button>
          }
        >
          {/* 仅在「空内容 + 响应中」显示停止；有内容时始终显示发送（消息入队） */}
          <button type="button" class="stop-btn" onClick={stop} aria-label="stop">
            ■
          </button>
        </Show>
      </form>

      <Sheet open={drawerOpen()} onOpenChange={setDrawerOpen}>
        <SheetContent
          side="right"
          class="w-4/5 max-w-xs gap-3 p-4 pb-[calc(1rem+env(safe-area-inset-bottom))]"
        >
          <SheetHeader>
            <SheetTitle class="text-base">Sessions</SheetTitle>
          </SheetHeader>
          <Button variant="outline" size="sm" onClick={newSession}>
            ＋ New session
          </Button>
          <div class="-mx-1 flex-1 overflow-y-auto px-1">
            <For each={sessions()}>
              {(s) => (
                <div
                  class={`item-card ${s.id === currentSession() ? "active" : ""}`}
                  onClick={() => switchSession(s.id)}
                >
                  <div class="item-title">{s.id.slice(0, 8)}</div>
                  <div class="item-sub">
                    {fmtRel(s.modifiedAt)} · {s.entries} messages
                  </div>
                </div>
              )}
            </For>
            <Show when={!sessions().length}>
              <div class="empty-note">no sessions yet</div>
            </Show>

            <div class="mt-4">
              <div class="flex items-center justify-between">
                <strong class="text-sm">MCP servers</strong>
                <Button variant="ghost" size="sm" class="h-7 text-xs" onClick={reconnectMcp}>
                  ⟳ Reconnect
                </Button>
              </div>
              <For each={mcpServers()}>
                {(s) => (
                  <div class="item-card">
                    <div class="item-title">{s.name}</div>
                    <div class="item-sub mcp-url">{s.url}</div>
                    <div class="item-sub">
                      timeout {s.timeoutMs ?? 30000}ms
                      <Show when={s.headers && Object.keys(s.headers ?? {}).length > 0}>
                        {" · headers: "}
                        {Object.keys(s.headers ?? {}).join(", ")}
                      </Show>
                    </div>
                    <Button
                      variant="ghost"
                      size="sm"
                      class="mt-1 h-7 text-xs text-muted-foreground"
                      onClick={() => removeMcpServer(s.name)}
                    >
                      Remove
                    </Button>
                  </div>
                )}
              </For>
              <form onSubmit={addMcpServer} class="mt-2 flex flex-col gap-1.5">
                <input
                  class="ask-input"
                  placeholder="name (e.g. docs)"
                  value={mcpName()}
                  onInput={(e) => setMcpName(e.currentTarget.value)}
                />
                <input
                  class="ask-input"
                  placeholder="https://…/mcp"
                  value={mcpUrl()}
                  onInput={(e) => setMcpUrl(e.currentTarget.value)}
                />
                <input
                  class="ask-input"
                  type="number"
                  placeholder="timeout ms (default 30000)"
                  value={mcpTimeout()}
                  onInput={(e) => setMcpTimeout(e.currentTarget.value)}
                />
                <textarea
                  class="ask-input"
                  rows="2"
                  placeholder={"headers (optional, one per line): Authorization: Bearer …"}
                  value={mcpHeaders()}
                  onInput={(e) => setMcpHeaders(e.currentTarget.value)}
                />
                <Button variant="outline" size="sm" type="submit">
                  Add server
                </Button>
              </form>
              <div class="item-sub mt-1">calls require approval · ⟳ Reconnect applies config changes</div>
            </div>

            <div class="mt-4">
              <strong class="text-sm">Skills</strong>
              <For each={skills()}>
                {(s) => (
                  <div class="item-card">
                    <div class="item-title">
                      {s.name}{" "}
                      <span class="item-sub">
                        {s.enabled ? "· on" : "· off"} · {s.version}
                      </span>
                    </div>
                    <Show when={s.description}>
                      <div class="item-sub">{s.description}</div>
                    </Show>
                    <div class="item-sub mcp-url">{s.checksum}</div>
                    <div class="mt-1 flex gap-1.5">
                      <Button
                        variant="ghost"
                        size="sm"
                        class="h-7 text-xs"
                        onClick={() => toggleSkill(s.id, !s.enabled)}
                      >
                        {s.enabled ? "Disable" : "Enable"}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        class="h-7 text-xs text-muted-foreground"
                        onClick={() => removeSkill(s.id)}
                      >
                        Remove
                      </Button>
                    </div>
                  </div>
                )}
              </For>
              <Show when={!skills().length}>
                <div class="empty-note">no skills installed</div>
              </Show>
              <form onSubmit={installSkill} class="mt-2 flex flex-col gap-1.5">
                <input
                  class="ask-input"
                  placeholder="https://github.com/owner/repo or SKILL.md URL"
                  value={skillUrl()}
                  onInput={(e) => setSkillUrl(e.currentTarget.value)}
                />
                <Button variant="outline" size="sm" type="submit" disabled={installingSkill()}>
                  {installingSkill() ? "Installing…" : "Install skill"}
                </Button>
              </form>
              <div class="item-sub mt-1">
                SKILL.md instructions inject into the system prompt · disabled = not injected · no code runs
              </div>
            </div>
          </div>
        </SheetContent>
      </Sheet>

      <Sheet open={filesOpen()} onOpenChange={setFilesOpen}>
        <SheetContent
          side="left"
          class="w-4/5 max-w-xs gap-3 p-4 pb-[calc(1rem+env(safe-area-inset-bottom))]"
        >
          <SheetHeader>
            <SheetTitle class="text-base">Workspace</SheetTitle>
          </SheetHeader>
          <Button variant="outline" size="sm" onClick={openFiles}>
            ⟳ Refresh
          </Button>
          <div class="-mx-1 flex-1 overflow-y-auto px-1">
            <For each={tree()}>
              {(t) => (
                <div
                  class={`file-item ${t.kind === "directory" ? "dir" : ""}`}
                  style={{
                    "margin-left": `${(t.path.split("/").length - 1) * 0.8}rem`,
                  }}
                  onClick={() => t.kind === "file" && previewFile(t.path)}
                >
                  {t.kind === "directory" ? "▸ " : ""}
                  {t.path.split("/").pop()}
                  {t.kind === "file" ? `  (${t.size}B)` : "/"}
                </div>
              )}
            </For>
            <Show when={!tree().length}>
              <div class="empty-note">workspace is empty</div>
            </Show>
          </div>
        </SheetContent>
      </Sheet>

      <Show when={preview()}>
        {(p) => (
          <div class="preview-overlay" onClick={() => setPreview(null)}>
            <div class="preview-panel" onClick={(e) => e.stopPropagation()}>
              <div class="preview-head">
                <strong>{p().path}</strong>
                <button class="icon-btn" onClick={() => setPreview(null)}>
                  ✕
                </button>
              </div>
              <pre class="preview-body">{p().content}</pre>
            </div>
          </div>
        )}
      </Show>
    </main>
  );
}

export default App;
