import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { createSignal, For, onCleanup, onMount, Show } from "solid-js";
import "./App.css";

type ChatItem = {
  role: "user" | "assistant" | "tool" | "status";
  text: string;
};

function App() {
  const [items, setItems] = createSignal<ChatItem[]>([
    { role: "status", text: "booting embedded pi agent…" },
  ]);
  const [input, setInput] = createSignal("");
  const [apiKey, setApiKey] = createSignal("");
  const [ready, setReady] = createSignal(false);

  const push = (item: ChatItem) => setItems((prev) => [...prev, item]);

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
          if (ev.messages > 0)
            push({ role: "status", text: `restored session (${ev.messages} messages)` });
          break;
        case "session_created":
          push({ role: "status", text: `new session ${String(ev.sessionId).slice(0, 8)}` });
          break;
        case "session_error":
          push({ role: "status", text: `session persist error: ${ev.error}` });
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
          });
          break;
        case "tool_execution_end": {
          const out =
            ev.result?.content
              ?.filter((c: any) => c.type === "text")
              .map((c: any) => c.text)
              .join("") ?? "";
          push({ role: "tool", text: `↳ ${String(out).slice(0, 300)}` });
          break;
        }
        default:
          break;
      }
    });
    onCleanup(un);

    try {
      await invoke("agent_init");
      // 重启恢复：boot 时 bundle 已从最新 JSONL 会话回放，这里拉历史渲染
      const h = JSON.parse(await invoke<string>("agent_history"));
      const msgs = (h.messages ?? []) as any[];
      if (msgs.length) {
        setItems(
          msgs.map((m) => {
            const text =
              typeof m.content === "string"
                ? m.content
                : (m.content ?? [])
                    .filter((c: any) => c.type === "text")
                    .map((c: any) => c.text)
                    .join("");
            if (m.role === "toolResult")
              return { role: "tool", text: `↳ ${text.slice(0, 200)}` } as ChatItem;
            if (m.role === "assistant")
              return { role: "assistant", text } as ChatItem;
            return { role: "user", text } as ChatItem;
          }),
        );
        push({
          role: "status",
          text: `history loaded — ${msgs.length} messages from previous run`,
        });
      }
    } catch (e) {
      push({ role: "status", text: `agent_init failed: ${e}` });
    }
  });

  async function saveKey(e: Event) {
    e.preventDefault();
    if (!apiKey().trim()) return;
    await invoke("set_creds", {
      provider: "anthropic",
      apiKey: apiKey().trim(),
    });
    setApiKey("");
    push({ role: "status", text: "API key saved (anthropic)" });
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

  return (
    <main
      class="container"
      style={{ display: "flex", "flex-direction": "column", height: "100vh" }}
    >
      <h1 style={{ "font-size": "1.1rem" }}>pi-mobile</h1>

      <Show when={!ready()}>
        <form class="row" onSubmit={saveKey}>
          <input
            type="password"
            placeholder="Anthropic API key…"
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
            </div>
          )}
        </For>
      </div>

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
