import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import "./App.css";

type ChatItem = {
  role: "user" | "assistant" | "tool" | "status";
  text: string;
  toolCallId?: string;
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

function fmtTime(millis: number): string {
  const d = new Date(millis);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getMonth() + 1}/${d.getDate()} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function App() {
  const [items, setItems] = createSignal<ChatItem[]>([
    { role: "status", text: "booting embedded pi agent…" },
  ]);
  const [input, setInput] = createSignal("");
  const [apiKey, setApiKey] = createSignal("");
  const [ready, setReady] = createSignal(false);
  const [approval, setApproval] = createSignal<Approval | null>(null);
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [sessions, setSessions] = createSignal<SessionMeta[]>([]);
  const [currentSession, setCurrentSession] = createSignal<string | null>(null);
  const [filesOpen, setFilesOpen] = createSignal(false);
  const [tree, setTree] = createSignal<TreeEntry[]>([]);
  const [preview, setPreview] = createSignal<{ path: string; content: string } | null>(null);

  const push = (item: ChatItem) => setItems((prev) => [...prev, item]);
  const updateItem = (toolCallId: string, patch: Partial<ChatItem>) =>
    setItems((prev) =>
      prev.map((it) => (it.toolCallId === toolCallId ? { ...it, ...patch } : it)),
    );

  function mapHistoryMessage(m: any): ChatItem {
    const text =
      typeof m.content === "string"
        ? m.content
        : (m.content ?? [])
            .filter((c: any) => c.type === "text")
            .map((c: any) => c.text)
            .join("");
    if (m.role === "toolResult")
      return { role: "tool", text: `↳ ${text.slice(0, 200)}` };
    if (m.role === "assistant") {
      // 纯 toolCall 消息没有文本块 —— 汇总为工具卡行，避免空气泡
      const calls = (m.content ?? [])
        .filter((c: any) => c.type === "toolCall")
        .map((c: any) => `⚒ ${c.name}(${JSON.stringify(c.arguments ?? {})})`);
      return { role: "assistant", text: text || calls.join("\n") };
    }
    return { role: "user", text };
  }

  /// 从 bundle 拉当前会话历史并渲染（boot 恢复 / 切换会话后共用）。
  async function loadHistory() {
    const h = JSON.parse(await invoke<string>("agent_history"));
    setCurrentSession(h.sessionId ?? null);
    const msgs = (h.messages ?? []) as any[];
    if (!msgs.length) {
      setItems([{ role: "status", text: "new session — say hi or ask pi to do something" }]);
      return;
    }
    setItems(msgs.map(mapHistoryMessage));
    push({ role: "status", text: `history loaded — ${msgs.length} messages` });
  }

  /// 查询某路径是否还有可回滚的备份，刷新对应工具卡。
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
    // agent 事件流（bundle → loopback → Rust emit → 这里）
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
          push({
            role: "status",
            text: `agent ready — tools: ${(ev.tools ?? []).join(", ")}`,
          });
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
          push({ role: "status", text: `BOOT ERROR: ${ev.error}` });
          break;
        case "agent_error":
          push({ role: "status", text: `ERROR: ${ev.error}` });
          break;
        case "agent_start":
          break;
        case "agent_end":
          break;
        case "message_start":
        case "message_update":
        case "message_end": {
          // 流式 assistant 消息：把 delta 汇总进最后一条 assistant 项
          const msg = ev.message ?? {};
          const text =
            msg.content
              ?.filter((c: any) => c.type === "text")
              .map((c: any) => c.text)
              .join("") ?? "";
          if (ev.type === "message_update" && !text) break;
          setItems((prev) => {
            const last = prev[prev.length - 1];
            if (last?.role === "assistant" && ev.type !== "message_start") {
              return [...prev.slice(0, -1), { ...last, text }];
            }
            return [...prev, { role: "assistant", text }];
          });
          break;
        }
        case "tool_execution_start":
          push({
            role: "tool",
            text: `⚒ ${ev.toolName}(${JSON.stringify(ev.args ?? {}).slice(0, 120)})`,
            toolCallId: ev.toolCallId,
            path: ev.args?.path,
          });
          break;
        case "tool_execution_end": {
          const out =
            ev.result?.content
              ?.filter((c: any) => c.type === "text")
              .map((c: any) => c.text)
              .join("") ?? "";
          updateItem(ev.toolCallId, { text: `↳ ${String(out).slice(0, 300)}` });
          // write 卡片查询备份（覆盖写才有）→ 显示回滚 chip
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
      // 重启恢复：bundle 已回放最新会话，这里拉历史渲染
      await loadHistory();
    } catch (e) {
      push({ role: "status", text: `agent_init failed: ${e}` });
    }
  });

  async function saveKey(e: Event) {
    e.preventDefault();
    if (!apiKey().trim()) return;
    // 默认模型 deepseek-v4-flash（agent-main.js DEFAULT_MODEL），凭证按 provider 名存
    await invoke("set_creds", {
      provider: "deepseek",
      apiKey: apiKey().trim(),
    });
    setApiKey("");
    push({ role: "status", text: "API key saved (deepseek)" });
  }

  async function send(e: Event) {
    e.preventDefault();
    const text = input().trim();
    if (!text || !ready()) return;
    setInput("");
    push({ role: "user", text });
    try {
      const r = await invoke<string>("agent_prompt", { text });
      if (r !== "started") push({ role: "status", text: `prompt kick: ${r}` });
    } catch (e) {
      push({ role: "status", text: `prompt failed: ${e}` });
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

  return (
    <main
      class="container"
      style={{ display: "flex", "flex-direction": "column", height: "100vh" }}
    >
      <div style={{ display: "flex", "align-items": "center", gap: "0.5rem" }}>
        <button
          style={{
            background: "none",
            border: "1px solid #3a4a5c",
            color: "#c7d4e0",
            "border-radius": "0.4rem",
            padding: "0.15rem 0.5rem",
            "font-size": "0.95rem",
          }}
          onClick={openDrawer}
        >
          ☰
        </button>
        <button
          style={{
            background: "none",
            border: "1px solid #3a4a5c",
            color: "#c7d4e0",
            "border-radius": "0.4rem",
            padding: "0.15rem 0.5rem",
            "font-size": "0.95rem",
          }}
          onClick={openFiles}
        >
          📁
        </button>
        <h1 style={{ "font-size": "1.1rem", flex: "1" }}>pi-mobile</h1>
        <Show when={currentSession()}>
          <span style={{ color: "#7d8b99", "font-size": "0.7rem" }}>
            {currentSession()!.slice(0, 8)}
          </span>
        </Show>
      </div>

      <Show when={!ready()}>
        <form class="row" onSubmit={saveKey}>
          <input
            type="password"
            placeholder="DeepSeek API key…"
            value={apiKey()}
            onInput={(e) => setApiKey(e.currentTarget.value)}
          />
          <button type="submit">Save</button>
        </form>
      </Show>

      <div
        id="chat"
        style={{
          flex: "1",
          overflow: "auto",
          "text-align": "left",
          padding: "0.5rem",
        }}
      >
        <For each={items()}>
          {(item) => (
            <div
              style={{
                margin: "0.4rem 0",
                padding: "0.45rem 0.6rem",
                "border-radius": "0.6rem",
                "white-space": "pre-wrap",
                "word-break": "break-word",
                "font-size": "0.85rem",
                ...(item.role === "user"
                  ? { background: "#2f6feb", color: "#fff" }
                  : item.role === "assistant"
                    ? { background: "#24313f" }
                    : item.role === "tool"
                      ? {
                          background: "#1c2530",
                          color: "#9fb3c8",
                          "font-family": "monospace",
                          "font-size": "0.75rem",
                        }
                      : { color: "#7d8b99", "font-style": "italic" }),
              }}
            >
              {item.text}
              <Show when={item.role === "tool" && item.path && (item.canRevert || item.reverted)}>
                <div style={{ "margin-top": "0.3rem" }}>
                  <Show
                    when={item.canRevert}
                    fallback={
                      <span style={{ color: "#5f7183" }}>↩ reverted</span>
                    }
                  >
                    <button
                      style={{
                        background: "#2c4a5e",
                        color: "#8ec6ff",
                        border: "none",
                        "border-radius": "0.4rem",
                        padding: "0.2rem 0.6rem",
                        "font-size": "0.72rem",
                      }}
                      onClick={() => revert(item)}
                    >
                      ↩ Revert
                    </button>
                  </Show>
                </div>
              </Show>
            </div>
          )}
        </For>
      </div>

      <Show when={approval()}>
        {(a) => (
          <div
            style={{
              border: "1px solid #3a4a5c",
              "border-radius": "0.6rem",
              margin: "0.3rem 0.5rem",
              padding: "0.5rem",
              background: "#1a232e",
              "max-height": "45vh",
              "overflow-y": "auto",
            }}
          >
            <div style={{ "font-size": "0.8rem", "font-weight": "bold" }}>
              ⚠ {a().tool} «{a().path}» — approve?
            </div>
            <Show when={a().diff}>
              <pre
                style={{
                  "font-family": "monospace",
                  "font-size": "0.68rem",
                  "line-height": "1.35",
                  "white-space": "pre-wrap",
                  "word-break": "break-all",
                  margin: "0.4rem 0",
                  padding: "0.4rem",
                  background: "#121922",
                  "border-radius": "0.4rem",
                }}
              >
                {a().diff.split("\n").map((line) => (
                  <div
                    style={
                      line.startsWith("+")
                        ? { color: "#7ce38b" }
                        : line.startsWith("-")
                          ? { color: "#ff8182" }
                          : { color: "#7d8b99" }
                    }
                  >
                    {line || " "}
                  </div>
                ))}
              </pre>
            </Show>
            <div style={{ display: "flex", gap: "0.5rem", "margin-top": "0.4rem" }}>
              <button
                style={{ flex: "1", background: "#5a3038", color: "#ff9ea0", border: "none", padding: "0.45rem", "border-radius": "0.4rem" }}
                onClick={() => decide("deny")}
              >
                Deny
              </button>
              <button
                style={{ flex: "1", background: "#2c4a5e", color: "#8ec6ff", border: "none", padding: "0.45rem", "border-radius": "0.4rem" }}
                onClick={() => decide("always")}
              >
                Always
              </button>
              <button
                style={{ flex: "1", background: "#1f4a33", color: "#8fe6a4", border: "none", padding: "0.45rem", "border-radius": "0.4rem" }}
                onClick={() => decide("allow")}
              >
                Allow
              </button>
            </div>
          </div>
        )}
      </Show>

      <Show when={drawerOpen()}>
        <div
          style={{
            position: "fixed",
            inset: "0",
            background: "rgba(0,0,0,0.45)",
            "z-index": "10",
          }}
          onClick={() => setDrawerOpen(false)}
        />
        <div
          style={{
            position: "fixed",
            top: "0",
            right: "0",
            bottom: "0",
            width: "82vw",
            "max-width": "22rem",
            background: "#141c26",
            "z-index": "11",
            padding: "0.8rem",
            "overflow-y": "auto",
            "border-left": "1px solid #3a4a5c",
          }}
        >
          <div style={{ display: "flex", "align-items": "center", "margin-bottom": "0.6rem" }}>
            <strong style={{ flex: "1" }}>Sessions</strong>
            <button
              style={{
                background: "#1f4a33",
                color: "#8fe6a4",
                border: "none",
                "border-radius": "0.4rem",
                padding: "0.3rem 0.7rem",
              }}
              onClick={newSession}
            >
              ＋ New
            </button>
          </div>
          <For each={sessions()}>
            {(s) => (
              <div
                style={{
                  padding: "0.5rem 0.6rem",
                  "border-radius": "0.5rem",
                  margin: "0.25rem 0",
                  cursor: "pointer",
                  background: s.id === currentSession() ? "#24313f" : "#1a232e",
                  border:
                    s.id === currentSession() ? "1px solid #2f6feb" : "1px solid #232f3d",
                }}
                onClick={() => switchSession(s.id)}
              >
                <div style={{ "font-size": "0.8rem", "font-family": "monospace" }}>
                  {s.id.slice(0, 8)}
                </div>
                <div style={{ "font-size": "0.7rem", color: "#7d8b99" }}>
                  {fmtTime(s.modifiedAt)} · {s.entries} messages
                </div>
              </div>
            )}
          </For>
          <Show when={!sessions().length}>
            <div style={{ color: "#7d8b99", "font-size": "0.8rem" }}>no sessions yet</div>
          </Show>
        </div>
      </Show>

      <Show when={filesOpen()}>
        <div
          style={{
            position: "fixed",
            inset: "0",
            background: "rgba(0,0,0,0.45)",
            "z-index": "10",
          }}
          onClick={() => setFilesOpen(false)}
        />
        <div
          style={{
            position: "fixed",
            top: "0",
            left: "0",
            bottom: "0",
            width: "82vw",
            "max-width": "22rem",
            background: "#141c26",
            "z-index": "11",
            padding: "0.8rem",
            "overflow-y": "auto",
            "border-right": "1px solid #3a4a5c",
          }}
        >
          <div style={{ display: "flex", "align-items": "center", "margin-bottom": "0.6rem" }}>
            <strong style={{ flex: "1" }}>Workspace</strong>
            <button
              style={{
                background: "none",
                border: "1px solid #3a4a5c",
                color: "#c7d4e0",
                "border-radius": "0.4rem",
                padding: "0.2rem 0.5rem",
              }}
              onClick={openFiles}
            >
              ⟳
            </button>
          </div>
          <For each={tree()}>
            {(t) => (
              <div
                style={{
                  padding: "0.3rem 0.4rem",
                  "border-radius": "0.4rem",
                  "font-size": "0.78rem",
                  "font-family": "monospace",
                  cursor: t.kind === "file" ? "pointer" : "default",
                  color: t.kind === "directory" ? "#8ec6ff" : "#c7d4e0",
                  "font-weight": t.kind === "directory" ? "bold" : "normal",
                  "margin-left": `${(t.path.split("/").length - 1) * 0.8}rem`,
                }}
                onClick={() => t.kind === "file" && previewFile(t.path)}
              >
                {t.kind === "directory" ? "▸ " : "  "}
                {t.path.split("/").pop()}
                {t.kind === "file" ? ` (${t.size}B)` : "/"}
              </div>
            )}
          </For>
          <Show when={!tree().length}>
            <div style={{ color: "#7d8b99", "font-size": "0.8rem" }}>workspace is empty</div>
          </Show>
        </div>
      </Show>

      <Show when={preview()}>
        {(p) => (
          <div
            style={{
              position: "fixed",
              inset: "0",
              background: "rgba(0,0,0,0.6)",
              "z-index": "20",
              display: "flex",
              "flex-direction": "column",
              padding: "0.8rem",
            }}
            onClick={() => setPreview(null)}
          >
            <div
              style={{
                background: "#141c26",
                "border-radius": "0.6rem",
                "border": "1px solid #3a4a5c",
                flex: "1",
                display: "flex",
                "flex-direction": "column",
                "min-height": "0",
              }}
              onClick={(e) => e.stopPropagation()}
            >
              <div
                style={{
                  display: "flex",
                  "align-items": "center",
                  padding: "0.5rem 0.7rem",
                  "border-bottom": "1px solid #232f3d",
                }}
              >
                <strong style={{ flex: "1", "font-size": "0.8rem", "font-family": "monospace" }}>
                  {p().path}
                </strong>
                <button
                  style={{
                    background: "none",
                    border: "1px solid #3a4a5c",
                    color: "#c7d4e0",
                    "border-radius": "0.4rem",
                    padding: "0.1rem 0.5rem",
                  }}
                  onClick={() => setPreview(null)}
                >
                  ✕
                </button>
              </div>
              <pre
                style={{
                  flex: "1",
                  overflow: "auto",
                  margin: "0",
                  padding: "0.6rem",
                  "font-family": "monospace",
                  "font-size": "0.72rem",
                  "line-height": "1.4",
                  "white-space": "pre-wrap",
                  "word-break": "break-all",
                  color: "#c7d4e0",
                }}
              >
                {p().content}
              </pre>
            </div>
          </div>
        )}
      </Show>

      <form class="row" onSubmit={send} style={{ "padding-bottom": "0.8rem" }}>
        <input
          placeholder={ready() ? "Ask pi to do something…" : "agent booting…"}
          disabled={!ready()}
          value={input()}
          onInput={(e) => setInput(e.currentTarget.value)}
        />
        <button type="submit" disabled={!ready() || !input().trim()}>
          Send
        </button>
      </form>
    </main>
  );
}

export default App;
