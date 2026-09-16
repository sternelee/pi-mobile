import { FiRotateCw } from "solid-icons/fi";
import { For, Show } from "solid-js";
import { Button } from "~/components/ui/button";
import type { McpServer } from "~/lib/types";

interface Props {
  mcpServers: () => McpServer[];
  mcpReady: () => Set<string>;
  onReconnect: () => void;
  onRemove: (name: string) => void;
  mcpName: () => string;
  setMcpName: (v: string) => void;
  mcpUrl: () => string;
  setMcpUrl: (v: string) => void;
  mcpTimeout: () => string;
  setMcpTimeout: (v: string) => void;
  mcpHeaders: () => string;
  setMcpHeaders: (v: string) => void;
  addMcpServer: (e: Event) => void;
  mcpPasteOpen: () => boolean;
  setMcpPasteOpen: (v: boolean) => void;
  mcpPaste: () => string;
  setMcpPaste: (v: string) => void;
  importMcpJson: () => void;
}

export function McpView(props: Props) {
  return (
    <>
      <div class="settings-section-title">MCP Servers</div>
      <div class="settings-subtitle">
        Streamable-HTTP servers — tools register as mcp__server__tool and always
        ask before running.
      </div>
      <div class="-mx-1 flex-1 overflow-y-auto px-1">
        <div class="flex items-center justify-between">
          <span />
          <Button
            variant="ghost"
            size="sm"
            class="h-7 text-xs"
            onClick={props.onReconnect}
          >
            <FiRotateCw size="0.9em" /> Reconnect
          </Button>
        </div>
        <For each={props.mcpServers()}>
          {(s) => (
            <div class="item-card">
              <div class="item-title">
                <span
                  class={`status-dot ${props.mcpReady().has(s.name) ? "ok" : "off"}`}
                  title={
                    props.mcpReady().has(s.name) ? "connected" : "not connected"
                  }
                />
                {s.name}
              </div>
              <div class="item-sub mcp-url">{s.url}</div>
              <div class="item-sub">
                timeout {s.timeoutMs ?? 30000}ms
                <Show
                  when={s.headers && Object.keys(s.headers ?? {}).length > 0}
                >
                  {" · headers: "}
                  {Object.keys(s.headers ?? {}).join(", ")}
                </Show>
              </div>
              <Button
                variant="ghost"
                size="sm"
                class="mt-1 h-7 text-xs text-muted-foreground"
                onClick={() => props.onRemove(s.name)}
              >
                Remove
              </Button>
            </div>
          )}
        </For>
        <form onSubmit={props.addMcpServer} class="mt-2 flex flex-col gap-1.5">
          <input
            class="ask-input"
            placeholder="name (e.g. docs)"
            value={props.mcpName()}
            onInput={(e) => props.setMcpName(e.currentTarget.value)}
          />
          <input
            class="ask-input"
            placeholder="https://…/mcp"
            value={props.mcpUrl()}
            onInput={(e) => props.setMcpUrl(e.currentTarget.value)}
          />
          <input
            class="ask-input"
            type="number"
            placeholder="timeout ms (default 30000)"
            value={props.mcpTimeout()}
            onInput={(e) => props.setMcpTimeout(e.currentTarget.value)}
          />
          <textarea
            class="ask-input"
            rows="2"
            placeholder={
              "headers (optional, one per line): Authorization: Bearer …"
            }
            value={props.mcpHeaders()}
            onInput={(e) => props.setMcpHeaders(e.currentTarget.value)}
          />
          <Button variant="outline" size="sm" type="submit">
            Add server
          </Button>
          <Button
            variant="ghost"
            size="sm"
            class="h-7 text-xs"
            onClick={() => props.setMcpPasteOpen(!props.mcpPasteOpen())}
          >
            {"{ }"} Paste JSON
          </Button>
          <Show when={props.mcpPasteOpen()}>
            <textarea
              class="ask-input"
              rows="4"
              placeholder={
                '{"name":"docs","url":"https://…/mcp","timeoutMs":30000,"headers":{}} 或 {"mcpServers":{…}}'
              }
              value={props.mcpPaste()}
              onInput={(e) => props.setMcpPaste(e.currentTarget.value)}
            />
            <Button variant="secondary" size="sm" onClick={props.importMcpJson}>
              Import JSON
            </Button>
          </Show>
        </form>
        <div class="item-sub mt-1">
          calls require approval · Reconnect applies config changes
        </div>
      </div>
    </>
  );
}
