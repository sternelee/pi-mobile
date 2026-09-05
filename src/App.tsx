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

type TreeEntry = {
  path: string;
  kind: "file" | "directory";
  size: number;
  mtimeMs: number;
};

const SUGGESTIONS = [
  "List my workspace files",
  "Create hello.py that prints a greeting",
  "What can you do?",
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
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [sessions, setSessions] = createSignal<SessionMeta[]>([]);
  const [currentSession, setCurrentSession] = createSignal<string | null>(null);
  const [filesOpen, setFilesOpen] = createSignal(false);
  const [tree, setTree] = createSignal<TreeEntry[]>([]);
  const [preview, setPreview] = createSignal<{ path: string; content: string } | null>(
    null,
  );
  const [stick, setStick] = createSignal(true);

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
    if (!text || !ready() || busy()) return;
    setInput("");
    if (textareaEl) textareaEl.style.height = "auto";
    setStick(true);
    push({ role: "user", text });
    try {
      await invoke("agent_prompt", { text });
    } catch (e) {
      push({ role: "status", text: `prompt failed: ${e}` });
    }
  }

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

  async function openDrawer() {
    setFilesOpen(false);
    setDrawerOpen(true);
    try {
      setSessions(JSON.parse(await invoke<string>("session_list")));
    } catch (e) {
      push({ role: "status", text: `session_list failed: ${e}` });
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
          <button class="icon-btn" onClick={openDrawer} aria-label="sessions">
            ☰
          </button>
          <button class="icon-btn" onClick={openFiles} aria-label="files">
            📁
          </button>
        </div>
        <h1 class="topbar-title">pi-mobile</h1>
        <div class="topbar-meta">
          <Show when={currentSession()}>{currentSession()!.slice(0, 8)}</Show>
        </div>
      </header>

      <Show when={!ready()}>
        <form class="keyform" onSubmit={saveKey}>
          <input
            type="password"
            placeholder="DeepSeek API key…"
            value={apiKey()}
            onInput={(e) => setApiKey(e.currentTarget.value)}
          />
          <button type="submit">Save</button>
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
                {(s) => <button class="chip" onClick={() => sendText(s)}>{s}</button>}
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
                      fallback={<span class="status-line">{it.text}</span>}
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
                  <div class={`tool-card ${toolState(it)}`}>
                    <button
                      class="tool-head"
                      onClick={() =>
                        updateItem(it.toolCallId!, { expanded: !it.expanded })
                      }
                    >
                      <span>
                        {it.pending ? "◌" : it.isError ? "✗" : "✓"}
                      </span>
                      <span class="tool-summary">
                        {it.toolName}({(it.argsText ?? "").slice(0, 90)})
                        {it.pending ? " …" : ""}
                      </span>
                      <span class="tool-caret">{it.expanded ? "▼" : "▶"}</span>
                    </button>
                    <Show when={it.expanded}>
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
                              <button
                                class="chip-revert"
                                onClick={() => revert(it)}
                              >
                                ↩ Revert
                              </button>
                            </Show>
                          </div>
                        </Show>
                      </div>
                    </Show>
                  </div>
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
              <button class="btn btn-deny" onClick={() => decide("deny")}>
                Deny
              </button>
              <button class="btn btn-always" onClick={() => decide("always")}>
                Always
              </button>
              <button class="btn btn-allow" onClick={() => decide("allow")}>
                Allow
              </button>
            </div>
          </div>
        )}
      </Show>

      <form class="composer" onSubmit={onSubmit}>
        <textarea
          ref={textareaEl}
          rows="1"
          placeholder={ready() ? "Ask pi to do something…" : "agent booting…"}
          disabled={!ready()}
          value={input()}
          onInput={(e) => {
            setInput(e.currentTarget.value);
            autoGrow();
          }}
          onKeyDown={onKeydown}
        />
        <Show
          when={!busy()}
          fallback={
            <button type="button" class="stop-btn" onClick={stop} aria-label="stop">
              ■
            </button>
          }
        >
          <button
            type="submit"
            class="send-btn"
            disabled={!ready() || !input().trim()}
            aria-label="send"
          >
            ➤
          </button>
        </Show>
      </form>

      <Show when={drawerOpen()}>
        <div class="overlay" onClick={() => setDrawerOpen(false)} />
        <div class="drawer drawer-right">
          <div class="drawer-head">
            <strong>Sessions</strong>
            <button class="icon-btn" onClick={newSession}>
              ＋ New
            </button>
          </div>
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
        </div>
      </Show>

      <Show when={filesOpen()}>
        <div class="overlay" onClick={() => setFilesOpen(false)} />
        <div class="drawer drawer-left">
          <div class="drawer-head">
            <strong>Workspace</strong>
            <button class="icon-btn" onClick={openFiles}>
              ⟳
            </button>
          </div>
          <For each={tree()}>
            {(t) => (
              <div
                class={`file-item ${t.kind === "directory" ? "dir" : ""}`}
                style={{ "margin-left": `${(t.path.split("/").length - 1) * 0.8}rem` }}
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
      </Show>

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
