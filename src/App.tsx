import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
  type Signal,
} from "solid-js";
import { createScrollPosition } from "@solid-primitives/scroll";
import { makePersisted } from "@solid-primitives/storage";
import { createMediaQuery } from "@solid-primitives/media";
import { Markdown } from "./ui/Markdown";
import { Badge } from "~/components/ui/badge";
import { Button } from "~/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "~/components/ui/dialog";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "~/components/ui/collapsible";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from "~/components/ui/sheet";
import { ToggleSwitch } from "~/components/ui/switch";
import {
  FiAlertTriangle,
  FiArrowDown,
  FiCheck,
  FiCheckSquare,
  FiChevronDown,
  FiChevronRight,
  FiCircle,
  FiCpu,
  FiFolder,
  FiKey,
  FiLoader,
  FiLock,
  FiMenu,
  FiPlay,
  FiPlus,
  FiRotateCw,
  FiSend,
  FiSettings,
  FiSquare,
  FiTarget,
  FiTrash2,
  FiX,
} from "solid-icons/fi";
import { BsOpenai } from "solid-icons/bs";
import {
  SiAnthropic,
  SiGooglegemini,
  SiOpenrouter,
  SiX,
} from "solid-icons/si";
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
  lastMessage?: string | null;
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

// AI provider / model（pi-ai createModels 目录，bundle 侧解析完整模型对象）
type ProviderModel = { id: string; name: string };
type ProviderInfo = { id: string; name: string; models: ProviderModel[] };
type CurrentModel = { provider: string; id: string; name: string };

// OAuth 订阅型 provider（bundle 侧 __pi_oauth_login 支持登录）
const OAUTH_PROVIDERS = new Set(["anthropic", "openai-codex", "kimi-coding", "xai", "openrouter"]);
// 图标随文字色/字号（solid-icons 默认 1em + currentColor）
const providerIcon = (id: string) =>
  id === "openai" || id === "openai-codex" ? (
    <BsOpenai size="1.1em" />
  ) : id === "openrouter" ? (
    <SiOpenrouter size="1.1em" />
  ) : id === "anthropic" ? (
    <SiAnthropic size="1.1em" />
  ) : id === "xai" ? (
    <SiX size="1.1em" />
  ) : id === "google-gemini" ? (
    <SiGooglegemini size="1.1em" />
  ) : (
    <FiCpu size="1.1em" />
  );

const UI_PROVIDERS: ProviderInfo[] = [
  { id: "openai", name: "OpenAI", models: [] },
  { id: "openrouter", name: "OpenRouter", models: [] },
  { id: "deepseek", name: "DeepSeek", models: [] },
  { id: "google-gemini", name: "Google Gemini", models: [] },
  { id: "anthropic", name: "Anthropic (Claude Pro/Max)", models: [] },
  { id: "openai-codex", name: "OpenAI Codex (ChatGPT)", models: [] },
  { id: "kimi-coding", name: "Kimi For Coding", models: [] },
  { id: "xai", name: "xAI (SuperGrok/X Premium)", models: [] },
];

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

// 技能自定义指令（/commit-it 等）：skills_applied 事件后从 bundle 回读。
// 面板合并展示；handleCommand 对匹配的未知命令透传 agent_prompt（bundle 展开）。
const [skillCmds, setSkillCmds] = createSignal<{ cmd: string; desc: string }[]>([]);
const allCommands = () => [...COMMANDS, ...skillCmds()];
const refreshSkillCmds = () => {
  invoke<string>("pi_call_global", { fnName: "__pi_commands", arg: "" })
    .then((r) =>
      setSkillCmds(
        JSON.parse(r).map((c: any) => ({ cmd: c.cmd, desc: c.description ?? c.name })),
      ),
    )
    .catch(() => setSkillCmds([]));
};

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
  const [input, setInput] = makePersisted<string, Signal<string>>(createSignal(""), {
    name: "pi-draft",
  });
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
  // autoContinue 状态（goal_auto_continue 事件驱动；null = 本回合非自动续跑）
  const [goalAuto, setGoalAuto] = createSignal<{ count: number; cap: number } | null>(null);
  const [todos, setTodos] = createSignal<{ tasks: TodoTask[]; nextId: number }>({
    tasks: [],
    nextId: 1,
  });
  const [todoOpen, setTodoOpen] = createSignal(false);

  // ── AI provider / model 选择（pi-ai createModels 目录驱动）──
  // 静态清单先渲染（产品定死 4 家）；runtime 的 providers_listed 事件带
  // 完整目录（含模型列表）后覆盖。OpenRouter 为动态目录，首次刷新才拉全量。
  const [providers, setProviders] = createSignal<ProviderInfo[]>(UI_PROVIDERS);
  const [selProvider, setSelProvider] = createSignal("");
  const [providerKey, setProviderKey] = createSignal("");
  const [keySaved, setKeySaved] = createSignal(false);
  const [loadingModels, setLoadingModels] = createSignal(false);
  const [currentModel, setCurrentModel] = createSignal<CurrentModel | null>(null);

  // 模型快选：优先当前生效模型的 provider，未选择时用抽屉里选中的 provider
  const refreshConfigured = async () => {
    const results = await Promise.all(
      UI_PROVIDERS.map((p) =>
        invoke<boolean>("has_creds", { provider: p.id }).catch(() => false),
      ),
    );
    setConfigured(new Set(UI_PROVIDERS.filter((_, i) => results[i]).map((p) => p.id)));
  };

  const pickerProvider = () => currentModel()?.provider || selProvider() || "openai";
  const pickerModels = () => providers().find((p) => p.id === pickerProvider())?.models ?? [];
  const openModelPicker = () => {
    setModelPickerOpen(true);
    if (pickerModels().length === 0) void loadModels(pickerProvider());
  };

  let chatEl: HTMLDivElement | undefined;
  // 滚动位置响应式跟踪（@solid-primitives/scroll）——滚离底部 >240px 时浮出跳底按钮
  const chatScroll = createScrollPosition(() => chatEl);
  const awayFromBottom = () => {
    const el = chatEl;
    if (!el) return 0;
    return el.scrollHeight - chatScroll.y - el.clientHeight;
  };
  const jumpToLatest = () => {
    chatEl?.scrollTo({
      top: chatEl.scrollHeight,
      behavior: reducedMotion() ? "auto" : "smooth",
    });
    setStick(true);
  };
  let textareaEl: HTMLTextAreaElement | undefined;

  // ── UI 2.0：动效偏好 / 会话搜索 / 设置页导航（@solid-primitives）──
  const reducedMotion = createMediaQuery("(prefers-reduced-motion: reduce)");
  const [sessionSearch, setSessionSearch] = createSignal("");
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [settingsView, setSettingsView] = createSignal<
    "providers" | "provider" | "mcp" | "skills" | "agent"
  >("providers");
  // 审批策略（write 基线 ask/auto）——Agent 设置页的开关
  const [approvalPolicy, setApprovalPolicy] = createSignal<"ask" | "auto">("ask");
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
  // 模型快选面板（composer 上方快捷条拉起）：当前 provider 的模型列表
  const [modelPickerOpen, setModelPickerOpen] = createSignal(false);
  // 已配置 key 的 provider 集合（LobeHub 式 provider 卡片状态标识）
  const [configured, setConfigured] = createSignal<Set<string>>(new Set());
  // MCP 服务器连接状态（mcp_ready / mcp_error 事件驱动）
  const [mcpReady, setMcpReady] = createSignal<Set<string>>(new Set());

  const push = (item: ChatItem) => setItems((prev) => [...prev, item]);
  const updateItem = (toolCallId: string, patch: Partial<ChatItem>) =>
    setItems((prev) =>
      prev.map((it) =>
        it.toolCallId === toolCallId ? { ...it, ...patch } : it,
      ),
    );

  const hasConversation = () =>
    items().some((i) => i.role === "user" || i.role === "assistant" || i.role === "tool");

  // 会话分组（Today / Yesterday / Earlier）+ 搜索过滤（id / 最后消息文本）
  const sessionGroups = createMemo(() => {
    const q = sessionSearch().trim().toLowerCase();
    const list = sessions().filter(
      (s) =>
        !q ||
        s.id.toLowerCase().includes(q) ||
        (s.lastMessage ?? "").toLowerCase().includes(q),
    );
    const dayStart = new Date();
    dayStart.setHours(0, 0, 0, 0);
    const yesterday = dayStart.getTime() - 86_400_000;
    const groups: { label: string; items: SessionMeta[] }[] = [
      { label: "Today", items: [] },
      { label: "Yesterday", items: [] },
      { label: "Earlier", items: [] },
    ];
    for (const s of list) {
      if (s.modifiedAt >= dayStart.getTime()) groups[0].items.push(s);
      else if (s.modifiedAt >= yesterday) groups[1].items.push(s);
      else groups[2].items.push(s);
    }
    return groups.filter((g) => g.items.length > 0);
  });

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
    // 持久化的模型选择先回显（agent_ready 后 __pi_model_current 会校正 name）
    try {
      const raw = await invoke<string>("get_default_model");
      const sel = JSON.parse(raw);
      if (sel?.provider) {
        setCurrentModel({ provider: sel.provider, id: sel.modelId, name: sel.modelId });
      }
    } catch {
      // 未配置：首启卡片引导选择
    }
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
          // 目录与当前模型回读（runtime 就绪后才有意义）
          refreshProviders();
          refreshSkillCmds();
          invoke<string>("pi_call_global", { fnName: "__pi_model_current", arg: "" })
            .then((r) => {
              const m = JSON.parse(r);
              if (m?.id) setCurrentModel(m);
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
        case "skills_applied":
          // 技能增删/启停后命令面板跟随（__pi_commands 回读）
          refreshSkillCmds();
          break;
        case "oauth_open_url":
          push({ role: "status", text: `browser opened — complete ${ev.provider ?? "provider"} sign-in` });
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
            push({ role: "status", text: `oauth: signed in to ${ev.provider ?? "provider"} — pick a model` });
            void refreshConfigured();
            invoke<string>("pi_call_global", { fnName: "__pi_providers_list", arg: "" }).catch(() => {});
          }
          break;
        case "mcp_ready":
          setMcpReady((prev) => new Set(prev).add(ev.server));
          push({ role: "status", text: `mcp ${ev.server} ready — ${(ev.tools ?? []).length} tools` });
          break;
        case "mcp_error":
          setMcpReady((prev) => {
            const next = new Set(prev);
            next.delete(ev.server);
            return next;
          });
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
        case "providers_listed":
          setProviders(ev.providers ?? []);
          break;
        case "providers_error":
          push({ role: "status", text: `providers: ${ev.error}` });
          break;
        case "models_listed":
          setLoadingModels(false);
          setProviders((ps) =>
            ps.map((p) => (p.id === ev.provider ? { ...p, models: ev.models ?? [] } : p)),
          );
          break;
        case "models_error":
          setLoadingModels(false);
          push({ role: "status", text: `models ${ev.provider}: ${ev.error}` });
          break;
        case "model_applied":
          setCurrentModel({
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
          push({ role: "status", text: "goal achieved — auto-continue stopped" });
          break;
        case "goal_error":
          setGoalAuto(null);
          push({ role: "status", text: `goal auto-continue failed: ${ev.error}` });
          break;
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

  // ── AI provider / model 选择流程 ──
  // 目录来自 bundle 的 providers_listed/models_listed 事件；key 存 Rust
  // creds（D4）；选择经 set_default_model 持久化、__pi_model_select 热切换。
  const providerModels = () => providers().find((p) => p.id === selProvider())?.models ?? [];
  const providerLabel = (id: string) => providers().find((p) => p.id === id)?.name ?? id;

  async function refreshProviders() {
    try {
      await invoke("pi_call_global", { fnName: "__pi_providers_list", arg: "" });
    } catch {
      // runtime 未就绪：保留静态清单，agent_ready 后会再拉
    }
  }

  async function chooseProvider(id: string) {
    setSelProvider(id);
    try {
      const has = await invoke<boolean>("has_creds", { provider: id });
      setKeySaved(has);
      if (has) setConfigured((prev) => new Set(prev).add(id));
    } catch {
      setKeySaved(false);
    }
    if (keySaved() && providerModels().length === 0) await loadModels(id);
  }

  async function saveProviderKey(e: Event) {
    e.preventDefault();
    const p = selProvider();
    if (!p || !providerKey().trim()) return;
    try {
      await invoke("set_creds", { provider: p, apiKey: providerKey().trim() });
      setProviderKey("");
      setKeySaved(true);
      setConfigured((prev) => new Set(prev).add(p));
      push({ role: "status", text: `API key saved (${p})` });
      await loadModels(p);
    } catch (err) {
      push({ role: "status", text: `save key failed: ${err}` });
    }
  }

  async function loadModels(id: string) {
    setLoadingModels(true);
    try {
      await invoke("pi_call_global", { fnName: "__pi_models_refresh", arg: id });
    } catch (err) {
      setLoadingModels(false);
      push({ role: "status", text: `model list failed: ${err}` });
    }
  }

  async function selectModel(p: string, m: ProviderModel) {
    try {
      const r = await invoke<string>("pi_call_global", {
        fnName: "__pi_model_select",
        arg: JSON.stringify({ provider: p, modelId: m.id }),
      });
      if (r !== "started") throw new Error(r);
      await invoke("set_default_model", { provider: p, modelId: m.id });
      setCurrentModel({ provider: p, id: m.id, name: m.name });
      setModelPickerOpen(false);
      push({ role: "status", text: `model set: ${m.name}` });
    } catch (err) {
      push({ role: "status", text: `model select failed: ${err}` });
    }
  }

  // Provider 选择节：首启卡片与抽屉共用（chooseProvider 自动拉已配置
  // provider 的模型列表；OpenRouter 动态目录首次刷新拉全量）。
  const providerSection = () => (
    <div class="flex flex-col gap-1.5">
      <div class="flex flex-wrap gap-1">
        <For each={providers()}>
          {(p) => (
            <Button
              variant={selProvider() === p.id ? "default" : "outline"}
              size="sm"
              class="h-7 text-xs"
              onClick={() => chooseProvider(p.id)}
            >
              {p.name}
            </Button>
          )}
        </For>
      </div>
      <Show when={selProvider()}>
        <Show
          when={!keySaved()}
          fallback={
            <div class="item-sub">
              API key configured ·{" "}
              <span class="underline" onClick={() => setKeySaved(false)}>
                replace
              </span>
            </div>
          }
        >
          <form class="flex flex-col gap-1.5" onSubmit={saveProviderKey}>
            <input
              class="ask-input"
              type="password"
              placeholder={`${providerLabel(selProvider())} API key…`}
              value={providerKey()}
              onInput={(e) => setProviderKey(e.currentTarget.value)}
            />
            <Button variant="outline" size="sm" type="submit">
              Save key & load models
            </Button>
          </form>
        </Show>
        <Show when={loadingModels()}>
          <div class="item-sub">loading models…</div>
        </Show>
        <For each={providerModels()}>
          {(m) => (
            <div class="item-card" onClick={() => selectModel(selProvider(), m)}>
              <div class="item-title">{m.name}</div>
              <div class="item-sub mcp-url">{m.id}</div>
            </div>
          )}
        </For>
      </Show>
    </div>
  );

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
    // 未知命令：匹配技能自定义指令 → 透传（bundle 按 skill 展开）；否则提示
    if (skillCmds().some((c) => c.cmd === cmd)) {
      try {
        await invoke("agent_prompt", { text });
      } catch (e) {
        push({ role: "status", text: `command failed: ${e}` });
      }
      return;
    }
    push({ role: "status", text: `unknown command ${cmd} — try ${allCommands().map((c) => c.cmd).join(", ")}` });
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
    t.status === "completed" ? (
      <FiCheck size="0.85em" />
    ) : t.status === "in_progress" ? (
      <FiLoader size="0.85em" />
    ) : (
      <FiCircle size="0.85em" />
    );

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
      setMcpReady((prev) => {
        const next = new Set(prev);
        next.delete(name);
        return next;
      });
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

  async function deleteSession(id: string) {
    try {
      // 当前会话先切空白：避免运行中 repo 的追加写把已删文件重建为无 header 孤儿
      if (id === currentSession()) {
        await invoke("session_new");
        setCurrentSession(null);
        setItems([{ role: "status", text: "session deleted — new session started" }]);
      } else {
        push({ role: "status", text: "session deleted" });
      }
      await invoke("session_delete", { id });
      setSessions((prev) => prev.filter((s) => s.id !== id));
    } catch (e) {
      push({ role: "status", text: `session delete failed: ${e}` });
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
            <FiMenu size="1.05em" />
          </Button>
        </div>
        <h1 class="topbar-title">pi-mobile</h1>
        <div class="topbar-meta">
          <Show when={currentSession()}>{currentSession()!.slice(0, 8)}</Show>
          <Button variant="secondary" size="icon" class="h-8 w-8" onClick={() => { void refreshConfigured(); setSettingsOpen(true); }} aria-label="settings">
            <FiSettings size="1.05em" />
          </Button>
        </div>
      </header>

      <Show when={goal()}>
        {(g) => (
          <div class="goal-banner">
            <span class="goal-text">
              <FiTarget size="0.95em" style={{ "vertical-align": "-0.1em" }} /> {g()}
              <Show when={goalAuto()}>
                <span class="goal-auto"> · auto {goalAuto()!.count}/{goalAuto()!.cap}</span>
              </Show>
            </span>
            <div class="goal-actions">
              <Show when={!busy()}>
                <button class="goal-btn" onClick={continueGoal}>
                  <FiPlay size="0.85em" /> Continue
                </button>
              </Show>
              <button class="goal-btn" onClick={clearGoal} aria-label="clear goal">
                <FiX size="0.9em" />
              </button>
            </div>
          </div>
        )}
      </Show>

      <Show when={!ready() || !currentModel()}>
        <div class="keyform provider-setup">
          <div class="item-sub">
            Choose an AI provider — the model list loads automatically after your
            key is saved.
          </div>
          {providerSection()}
        </div>
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
                        {it.pending ? "…" : it.isError ? "!" : <FiCheck size="0.9em" />}
                      </Badge>
                      <span class="tool-summary">
                        {it.toolName}({(it.argsText ?? "").slice(0, 90)})
                        {it.pending ? " …" : ""}
                      </span>
                      <span class="tool-caret">
                        {it.expanded ? <FiChevronDown /> : <FiChevronRight />}
                      </span>
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

      <Show when={awayFromBottom() > 240}>
        <button class="jump-btn" onClick={jumpToLatest} aria-label="jump to latest">
          <FiArrowDown size="1em" />
        </button>
      </Show>

      {/* 会话底部快捷条（ChatGPT 式）：文件 / 模型快选 / Todos */}
      <Show when={ready()}>
        <div class="quick-bar">
          <button class="quick-chip" onClick={openFiles}>
            <FiFolder size="0.95em" /> <span>Files</span>
          </button>
          <button class="quick-chip" onClick={openModelPicker}>
            <FiCpu size="0.95em" /> <span>{currentModel()?.name ?? "Model"}</span>
            <span class="quick-caret">
              <FiChevronDown size="0.8em" />
            </span>
          </button>
          <button class="quick-chip" onClick={() => setTodoOpen(!todoOpen())}>
            <FiCheckSquare size="0.95em" /> <span>Todos</span>
          </button>
        </div>
      </Show>

      <Show when={approval()}>
        {(a) => (
          <div class="approval">
            <div class="approval-title">
              <FiAlertTriangle size="0.95em" style={{ "vertical-align": "-0.12em" }} />{" "}
              {a().tool} «{a().path}» — approve?
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
          <For each={allCommands()}>
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
                <FiPlay size="0.85em" /> Approve &amp; run
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
              <FiX size="0.9em" />
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
        <div class="composer-pill">
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
                <FiSend size="1em" />
              </button>
            }
          >
            {/* 仅在「空内容 + 响应中」显示停止；有内容时始终显示发送（消息入队） */}
            <button type="button" class="stop-btn" onClick={stop} aria-label="stop">
              <FiSquare size="0.95em" />
            </button>
          </Show>
        </div>
      </form>

      <Sheet open={drawerOpen()} onOpenChange={setDrawerOpen}>
        <SheetContent
          side="left"
          class="sheet-safe w-4/5 max-w-xs gap-3 p-4 pb-[calc(1rem+env(safe-area-inset-bottom))]"
        >
          <SheetHeader>
            <SheetTitle class="text-base">Sessions</SheetTitle>
          </SheetHeader>
          <button class="new-chat-btn" onClick={newSession}>
            <FiPlus size="1em" /> New chat
          </button>
          <input
            class="session-search"
            placeholder="Search sessions…"
            value={sessionSearch()}
            onInput={(e) => setSessionSearch(e.currentTarget.value)}
          />
          <div class="-mx-1 flex-1 overflow-y-auto px-1">
            <For each={sessionGroups()}>
              {(grp) => (
                <>
                  <div class="session-group-label">{grp.label}</div>
                  <For each={grp.items}>
                    {(s) => (
                      <div
                        class={`item-card session-item ${s.id === currentSession() ? "active" : ""}`}
                        onClick={() => switchSession(s.id)}
                      >
                        <div class="item-body">
                          <div class="item-title">{s.lastMessage || s.id.slice(0, 8)}</div>
                          <div class="item-sub">
                            {fmtRel(s.modifiedAt)} · {s.entries} messages
                          </div>
                        </div>
                        <button
                          type="button"
                          class="session-delete-btn"
                          aria-label="delete session"
                          onClick={(e) => {
                            e.stopPropagation();
                            void deleteSession(s.id);
                          }}
                        >
                          <FiTrash2 size="0.95em" />
                        </button>
                      </div>
                    )}
                  </For>
                </>
              )}
            </For>
            <Show when={!sessionGroups().length}>
              <div class="empty-note">no sessions match</div>
            </Show>
          </div>

          <div class="mt-auto">
            <div
              class="settings-row"
              onClick={() => {
                setSettingsView("providers");
                void refreshConfigured();
                setDrawerOpen(false);
                setSettingsOpen(true);
              }}
            >
              <span class="settings-icon-chip">
                <FiSettings size="1.05em" />
              </span>
              <div class="settings-row-body">
                <div class="settings-row-title">Settings</div>
                <div class="settings-row-sub">AI model · MCP servers · Skills</div>
              </div>
              <span class="settings-chevron">
                <FiChevronRight size="1em" />
              </span>
            </div>
          </div>
        </SheetContent>
      </Sheet>

      <Sheet open={settingsOpen()} onOpenChange={setSettingsOpen}>
        <SheetContent
          side="right"
          class="sheet-safe w-full max-w-md gap-3 p-4 pb-[calc(1rem+env(safe-area-inset-bottom))]"
        >
          <SheetHeader>
            <SheetTitle class="text-base">Settings</SheetTitle>
          </SheetHeader>
          <div class="settings-tabs">
            <button
              class={`settings-tab ${settingsView() === "providers" || settingsView() === "provider" ? "active" : ""}`}
              onClick={() => setSettingsView("providers")}
            >
              Providers
            </button>
            <button
              class={`settings-tab ${settingsView() === "mcp" ? "active" : ""}`}
              onClick={() => setSettingsView("mcp")}
            >
              MCP
            </button>
            <button
              class={`settings-tab ${settingsView() === "skills" ? "active" : ""}`}
              onClick={() => setSettingsView("skills")}
            >
              Skills
            </button>
            <button
              class={`settings-tab ${settingsView() === "agent" ? "active" : ""}`}
              onClick={() => {
                setSettingsView("agent");
                invoke<string>("approval_policy_get")
                  .then((p) => setApprovalPolicy(p === "auto" ? "auto" : "ask"))
                  .catch(() => {});
              }}
            >
              Agent
            </button>
          </div>

          <Show when={settingsView() === "providers"}>
            <div class="-mx-1 flex-1 overflow-y-auto px-1">
              <div class="settings-subtitle">
                Provider catalogs come from the pi-ai models registry. Tap a
                provider to set its API key and pick a model.
              </div>
              <div class="flex flex-col gap-2">
                <For each={providers()}>
                  {(p) => (
                    <div
                      class="settings-row"
                      onClick={() => {
                        void chooseProvider(p.id);
                        setSettingsView("provider");
                      }}
                    >
                      <span class="settings-icon-chip">{providerIcon(p.id)}</span>
                      <div class="settings-row-body">
                        <div class="settings-row-title">
                          {p.name}
                          <Show when={currentModel()?.provider === p.id}>
                            <span class="provider-badge">active</span>
                          </Show>
                        </div>
                        <div class="settings-row-sub">
                          {p.models.length} models ·{" "}
                          {configured().has(p.id) ? "API key set" : "no key"}
                        </div>
                      </div>
                      <span class="settings-chevron">
                        <FiChevronRight size="1em" />
                      </span>
                    </div>
                  )}
                </For>
              </div>
              <div class="settings-footer">pi-mobile · sessions stay on this device</div>
            </div>
          </Show>

          <Show when={settingsView() === "provider"}>
            <div class="settings-section-title">{providerLabel(selProvider())}</div>
            <div class="settings-subtitle">
              Tap a model to make it the active model — applies immediately and
              persists across restarts.
            </div>
            <Show when={OAUTH_PROVIDERS.has(selProvider())}>
              <div class="item-card">
                <div class="item-title">
                  <FiLock size="0.95em" style={{ "vertical-align": "-0.12em" }} /> Subscription
                  sign-in
                </div>
                <div class="item-sub">
                  Opens the provider's login page in your browser and returns
                  via the pimobile:// deep link or a local callback — no API
                  key needed.
                </div>
                <Button
                  variant="outline"
                  size="sm"
                  class="mt-1"
                  disabled={busy()}
                  onClick={() => {
                    void invoke("pi_call_global", {
                      fnName: "__pi_oauth_login",
                      arg: selProvider(),
                    }).catch((e) => push({ role: "status", text: `oauth login failed: ${e}` }));
                    push({ role: "status", text: `signing in with ${providerLabel(selProvider())}…` });
                  }}
                >
                  Sign in with {providerLabel(selProvider())}
                </Button>
              </div>
            </Show>
            <div class="-mx-1 flex-1 overflow-y-auto px-1">
              <Show
                when={!keySaved()}
                fallback={
                  <div class="item-card">
                    <div class="item-title">
                      <FiKey size="0.95em" style={{ "vertical-align": "-0.12em" }} /> API key
                      configured
                    </div>
                    <div class="item-sub">
                      tap <span class="underline">replace</span> below to change it
                    </div>
                    <Button
                      variant="ghost"
                      size="sm"
                      class="mt-1 h-7 text-xs text-muted-foreground"
                      onClick={() => setKeySaved(false)}
                    >
                      Replace key
                    </Button>
                  </div>
                }
              >
                <form class="mb-2 flex flex-col gap-1.5" onSubmit={saveProviderKey}>
                  <input
                    class="ask-input"
                    type="password"
                    placeholder={`${providerLabel(selProvider())} API key…`}
                    value={providerKey()}
                    onInput={(e) => setProviderKey(e.currentTarget.value)}
                  />
                  <Button variant="outline" size="sm" type="submit">
                    Save key & load models
                  </Button>
                </form>
              </Show>
              <div class="flex items-center justify-between">
                <span class="item-sub">{providerModels().length} models</span>
                <Button
                  variant="ghost"
                  size="sm"
                  class="h-7 text-xs"
                  onClick={() => loadModels(selProvider())}
                >
                  <FiRotateCw size="0.9em" /> Refresh
                </Button>
              </div>
              <Show when={loadingModels()}>
                <div class="empty-note">loading models…</div>
              </Show>
              <For each={providerModels()}>
                {(m) => (
                  <div
                    class={`item-card ${
                      currentModel()?.provider === selProvider() && currentModel()?.id === m.id
                        ? "active"
                        : ""
                    }`}
                    onClick={() => selectModel(selProvider(), m)}
                  >
                    <div class="item-title">
                      <Show
                        when={currentModel()?.provider === selProvider() && currentModel()?.id === m.id}
                      >
                        <span class="model-check">
                          <FiCheck size="0.9em" />
                        </span>
                      </Show>
                      {m.name}
                    </div>
                    <div class="item-sub mcp-url">{m.id}</div>
                  </div>
                )}
              </For>
            </div>
          </Show>

          <Show when={settingsView() === "mcp"}>
            <div class="settings-section-title">MCP Servers</div>
            <div class="settings-subtitle">
              Streamable-HTTP servers — tools register as mcp__server__tool and
              always ask before running.
            </div>
            <div class="-mx-1 flex-1 overflow-y-auto px-1">
              <div class="flex items-center justify-between">
                <span />
                <Button variant="ghost" size="sm" class="h-7 text-xs" onClick={reconnectMcp}>
                  <FiRotateCw size="0.9em" /> Reconnect
                </Button>
              </div>
              <For each={mcpServers()}>
                {(s) => (
                  <div class="item-card">
                    <div class="item-title">
                      <span
                        class={`status-dot ${mcpReady().has(s.name) ? "ok" : "off"}`}
                        title={mcpReady().has(s.name) ? "connected" : "not connected"}
                      />
                      {s.name}
                    </div>
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
              <div class="item-sub mt-1">calls require approval · Reconnect applies config changes</div>
            </div>
          </Show>

          <Show when={settingsView() === "skills"}>
            <div class="settings-section-title">Skills</div>
            <div class="settings-subtitle">
              SKILL.md packages whose instructions are injected into the system
              prompt — no code runs on this device.
            </div>
            <div class="-mx-1 flex-1 overflow-y-auto px-1">
              <For each={skills()}>
                {(s) => (
                  <div class="item-card skill-row">
                    <div class="skill-row-body">
                      <div class="item-title">{s.name}</div>
                      <Show when={s.description}>
                        <div class="item-sub">{s.description}</div>
                      </Show>
                      <div class="item-sub mcp-url">
                        v{s.version} · {s.enabled ? "injected" : "not injected"}
                      </div>
                      <Button
                        variant="ghost"
                        size="sm"
                        class="h-7 text-xs text-muted-foreground"
                        onClick={() => removeSkill(s.id)}
                      >
                        Remove
                      </Button>
                    </div>
                    <ToggleSwitch on={s.enabled} onChange={() => toggleSkill(s.id, !s.enabled)} />
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
          </Show>

          <Show when={settingsView() === "agent"}>
            <div class="settings-section-title">Agent behavior</div>
            <div class="settings-subtitle">
              Approval gates protect the on-device workspace. MCP tools always
              ask regardless of this setting.
            </div>
            <div class="item-card flex items-center justify-between gap-3">
              <div>
                <div class="item-title">Approve file changes</div>
                <div class="item-sub">
                  {approvalPolicy() === "ask"
                    ? "write / edit / mkdir ask before running"
                    : "write / edit / mkdir run without asking"}
                </div>
              </div>
              <ToggleSwitch
                on={approvalPolicy() === "ask"}
                onChange={(next) => void setApprovalPolicyPersist(next ? "ask" : "auto")}
              />
            </div>
            <div class="item-card">
              <div class="item-title">Never approved without asking</div>
              <div class="item-sub">
                bash-style command execution does not exist in this build — the
                agent can only touch the sandboxed workspace.
              </div>
            </div>
            <div class="settings-footer">pi-mobile · sessions stay on this device</div>
          </Show>
        </SheetContent>
      </Sheet>

      <Sheet open={modelPickerOpen()} onOpenChange={setModelPickerOpen}>
        <SheetContent
          side="bottom"
          class="sheet-safe max-h-[70vh] gap-2 rounded-t-2xl p-4 pb-[calc(1rem+env(safe-area-inset-bottom))]"
        >
          <SheetHeader>
            <SheetTitle class="text-base">Model — {providerLabel(pickerProvider())}</SheetTitle>
          </SheetHeader>
          <div
            class="settings-row"
            onClick={() => {
              setModelPickerOpen(false);
              setSettingsView("providers");
              void refreshConfigured();
              setSettingsOpen(true);
            }}
          >
            <span class="settings-icon-chip">
              <FiCpu size="1.05em" />
            </span>
            <div class="settings-row-body">
              <div class="settings-row-title">Change provider</div>
              <div class="settings-row-sub">OpenAI · OpenRouter · DeepSeek · Gemini</div>
            </div>
            <span class="settings-chevron">
              <FiChevronRight size="1em" />
            </span>
          </div>
          <div class="-mx-1 flex-1 overflow-y-auto px-1">
            <Show when={pickerModels().length > 0} fallback={<div class="empty-note">loading models…</div>}>
              <For each={pickerModels()}>
                {(m) => (
                  <div
                    class={`item-card ${currentModel()?.id === m.id ? "active" : ""}`}
                    onClick={() => selectModel(pickerProvider(), m)}
                  >
                    <div class="item-title">
                      <Show when={currentModel()?.id === m.id}>
                        <span class="model-check">
                          <FiCheck size="0.9em" />
                        </span>
                      </Show>
                      {m.name}
                    </div>
                    <div class="item-sub mcp-url">{m.id}</div>
                  </div>
                )}
              </For>
            </Show>
          </div>
        </SheetContent>
      </Sheet>

      <Sheet open={filesOpen()} onOpenChange={setFilesOpen}>
        <SheetContent
          side="left"
          class="sheet-safe w-4/5 max-w-xs gap-3 p-4 pb-[calc(1rem+env(safe-area-inset-bottom))]"
        >
          <SheetHeader>
            <SheetTitle class="text-base">Workspace</SheetTitle>
          </SheetHeader>
          <Button variant="outline" size="sm" onClick={openFiles}>
            <FiRotateCw size="0.9em" /> Refresh
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
          <Dialog open={true} onOpenChange={(o) => !o && setPreview(null)}>
            <DialogContent class="w-[95vw] max-w-2xl gap-2 p-4">
              <DialogHeader>
                <DialogTitle class="truncate font-mono text-sm">{p().path}</DialogTitle>
                <DialogDescription>workspace file preview (read-only)</DialogDescription>
              </DialogHeader>
              <pre class="preview-body max-h-[65vh] overflow-auto">{p().content}</pre>
            </DialogContent>
          </Dialog>
        )}
      </Show>
    </main>
  );
}

export default App;
