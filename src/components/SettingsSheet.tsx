import { Show } from "solid-js";
import { AgentView } from "~/components/settings/AgentView";
import { McpView } from "~/components/settings/McpView";
import { ProvidersView } from "~/components/settings/ProvidersView";
import { ProviderView } from "~/components/settings/ProviderView";
import { SkillsView } from "~/components/settings/SkillsView";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from "~/components/ui/sheet";
import type {
  CurrentModel,
  McpServer,
  NativeCapability,
  ProviderInfo,
  ProviderModel,
  SettingsView as SettingsViewType,
  SkillMeta,
} from "~/lib/types";

interface Props {
  open: () => boolean;
  onOpenChange: (open: boolean) => void;
  view: () => SettingsViewType;
  setView: (v: SettingsViewType) => void;
  onOpenAgentTab: () => void;
  // ProvidersView / ProviderView
  busy: () => boolean;
  providers: () => ProviderInfo[];
  currentModel: () => CurrentModel | null;
  configured: () => Set<string>;
  onOpenProvider: (id: string) => void;
  selProvider: () => string;
  providerLabel: (id: string) => string;
  providerKey: () => string;
  setProviderKey: (v: string) => void;
  saveProviderKey: (e: Event) => void;
  keySaved: () => boolean;
  setKeySaved: (v: boolean) => void;
  loadingModels: () => boolean;
  loadModels: (id: string) => void;
  providerModels: () => ProviderModel[];
  selectModel: (p: string, m: ProviderModel) => void;
  onOAuthLogin: () => void;
  // McpView
  mcpServers: () => McpServer[];
  mcpReady: () => Set<string>;
  onReconnectMcp: () => void;
  onRemoveMcp: (name: string) => void;
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
  // SkillsView
  skills: () => SkillMeta[];
  onRemoveSkill: (id: string) => void;
  onToggleSkill: (id: string, enabled: boolean) => void;
  skillUrl: () => string;
  setSkillUrl: (v: string) => void;
  installingSkill: () => boolean;
  installSkill: (e: Event) => void;
  // AgentView
  approvalPolicy: () => "ask" | "auto";
  onSetApprovalPolicy: (next: "ask" | "auto") => void;
  nativeCaps: () => NativeCapability[];
  nativeErr: () => string;
  onRequestPermission: (cap: string) => void;
}

export function SettingsSheet(props: Props) {
  return (
    <Sheet open={props.open()} onOpenChange={props.onOpenChange}>
      <SheetContent
        side="right"
        class="sheet-safe w-full max-w-md gap-3 p-4 pb-[calc(1rem+env(safe-area-inset-bottom))]"
      >
        <SheetHeader>
          <SheetTitle class="text-base">Settings</SheetTitle>
        </SheetHeader>
        <div class="settings-tabs">
          <button
            type="button"
            class={`settings-tab ${props.view() === "providers" || props.view() === "provider" ? "active" : ""}`}
            onClick={() => props.setView("providers")}
          >
            Providers
          </button>
          <button
            type="button"
            class={`settings-tab ${props.view() === "mcp" ? "active" : ""}`}
            onClick={() => props.setView("mcp")}
          >
            MCP
          </button>
          <button
            type="button"
            class={`settings-tab ${props.view() === "skills" ? "active" : ""}`}
            onClick={() => props.setView("skills")}
          >
            Skills
          </button>
          <button
            type="button"
            class={`settings-tab ${props.view() === "agent" ? "active" : ""}`}
            onClick={props.onOpenAgentTab}
          >
            Agent
          </button>
        </div>

        <Show when={props.view() === "providers"}>
          <ProvidersView
            providers={props.providers}
            currentModel={props.currentModel}
            configured={props.configured}
            onOpenProvider={props.onOpenProvider}
          />
        </Show>

        <Show when={props.view() === "provider"}>
          <ProviderView
            busy={props.busy}
            selProvider={props.selProvider}
            providerLabel={props.providerLabel}
            providerKey={props.providerKey}
            setProviderKey={props.setProviderKey}
            saveProviderKey={props.saveProviderKey}
            keySaved={props.keySaved}
            setKeySaved={props.setKeySaved}
            loadingModels={props.loadingModels}
            loadModels={props.loadModels}
            providerModels={props.providerModels}
            currentModel={props.currentModel}
            selectModel={props.selectModel}
            onOAuthLogin={props.onOAuthLogin}
          />
        </Show>

        <Show when={props.view() === "mcp"}>
          <McpView
            mcpServers={props.mcpServers}
            mcpReady={props.mcpReady}
            onReconnect={props.onReconnectMcp}
            onRemove={props.onRemoveMcp}
            mcpName={props.mcpName}
            setMcpName={props.setMcpName}
            mcpUrl={props.mcpUrl}
            setMcpUrl={props.setMcpUrl}
            mcpTimeout={props.mcpTimeout}
            setMcpTimeout={props.setMcpTimeout}
            mcpHeaders={props.mcpHeaders}
            setMcpHeaders={props.setMcpHeaders}
            addMcpServer={props.addMcpServer}
            mcpPasteOpen={props.mcpPasteOpen}
            setMcpPasteOpen={props.setMcpPasteOpen}
            mcpPaste={props.mcpPaste}
            setMcpPaste={props.setMcpPaste}
            importMcpJson={props.importMcpJson}
          />
        </Show>

        <Show when={props.view() === "skills"}>
          <SkillsView
            skills={props.skills}
            onRemove={props.onRemoveSkill}
            onToggle={props.onToggleSkill}
            skillUrl={props.skillUrl}
            setSkillUrl={props.setSkillUrl}
            installingSkill={props.installingSkill}
            installSkill={props.installSkill}
          />
        </Show>

        <Show when={props.view() === "agent"}>
          <AgentView
            approvalPolicy={props.approvalPolicy}
            onSetApprovalPolicy={props.onSetApprovalPolicy}
            nativeCaps={props.nativeCaps}
            nativeErr={props.nativeErr}
            onRequestPermission={props.onRequestPermission}
          />
        </Show>
      </SheetContent>
    </Sheet>
  );
}
