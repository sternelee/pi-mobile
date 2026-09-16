import { For, Show } from "solid-js";
import { Button } from "~/components/ui/button";
import { ToggleSwitch } from "~/components/ui/switch";
import type { NativeCapability } from "~/lib/types";

interface Props {
  approvalPolicy: () => "ask" | "auto";
  onSetApprovalPolicy: (next: "ask" | "auto") => void;
  nativeCaps: () => NativeCapability[];
  nativeErr: () => string;
  onRequestPermission: (cap: string) => void;
}

export function AgentView(props: Props) {
  return (
    <div class="-mx-1 flex-1 overflow-y-auto px-1">
      <div class="settings-section-title">Agent behavior</div>
      <div class="settings-subtitle">
        Approval gates protect the on-device workspace. MCP tools always ask
        regardless of this setting.
      </div>
      <div class="item-card flex items-center justify-between gap-3">
        <div>
          <div class="item-title">Approve file changes</div>
          <div class="item-sub">
            {props.approvalPolicy() === "ask"
              ? "write / edit / mkdir ask before running"
              : "write / edit / mkdir run without asking"}
          </div>
        </div>
        <ToggleSwitch
          on={props.approvalPolicy() === "ask"}
          onChange={(next) =>
            void props.onSetApprovalPolicy(next ? "ask" : "auto")
          }
        />
      </div>
      <div class="item-card">
        <div class="item-title">Never approved without asking</div>
        <div class="item-sub">
          bash-style command execution does not exist in this build — the agent
          can only touch the sandboxed workspace.
        </div>
      </div>

      {/* 设备能力并入本页：审批策略管的是「工作区内的文件改动」，
          设备能力管的是「工作区外的真实用户数据」—— 两者都是 agent
          的权限边界，放同一页才不会让用户以为还有第二个开关组。
          清单与权限态的单一真源是 Rust 侧 native::CAPABILITIES。 */}
      <div class="settings-section-title">Device access</div>
      <div class="settings-subtitle">
        What the agent can reach outside the sandboxed workspace. Reading needs
        no prompt; anything that changes device state still asks.
      </div>
      <Show when={props.nativeErr()}>
        <div class="item-card">
          <div class="item-title">Could not load capabilities</div>
          <div class="item-sub">{props.nativeErr()}</div>
        </div>
      </Show>
      <For each={props.nativeCaps()}>
        {(cap) => (
          <div class="item-card cap-item flex items-center justify-between gap-3">
            <div class="min-w-0">
              <div class="cap-item-title">{cap.title}</div>
              <div class="cap-item-tools">{cap.tools.join(", ")}</div>
              <div class="cap-item-detail">{cap.detail}</div>
              <Show when={!cap.supported}>
                <div class="cap-item-note">Not available on this platform</div>
              </Show>
            </div>
            <Show
              when={
                cap.supported &&
                cap.needsPermission &&
                cap.permission !== "granted"
              }
              fallback={
                <span class="item-sub">
                  {cap.permission === "granted" ? "Allowed" : "—"}
                </span>
              }
            >
              <Button
                variant="secondary"
                size="sm"
                class="h-7 text-xs"
                onClick={() => void props.onRequestPermission(cap.id)}
              >
                {cap.permission === "denied" ? "Open Settings" : "Allow"}
              </Button>
            </Show>
          </div>
        )}
      </For>
      <div class="settings-footer">
        The agent never sends your location, clipboard or contacts anywhere on
        its own — only to the model when a tool call reads it.
      </div>
    </div>
  );
}
