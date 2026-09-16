import {
  FiChevronRight,
  FiEye,
  FiPlus,
  FiSettings,
  FiTrash2,
} from "solid-icons/fi";
import { For, Show } from "solid-js";
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from "~/components/ui/sheet";
import { activateOnKey } from "~/lib/a11y";
import { fmtRel } from "~/lib/format";
import type { SessionMeta } from "~/lib/types";

interface SessionGroup {
  label: string;
  items: SessionMeta[];
}

interface Props {
  open: () => boolean;
  onOpenChange: (open: boolean) => void;
  onNewSession: () => void;
  sessionSearch: () => string;
  setSessionSearch: (v: string) => void;
  sessionGroups: () => SessionGroup[];
  currentSession: () => string | null;
  onSwitchSession: (id: string) => void;
  onDeleteSession: (id: string) => void;
  onOpenPreview: () => void;
  onOpenSettings: () => void;
}

export function SessionDrawer(props: Props) {
  return (
    <Sheet open={props.open()} onOpenChange={props.onOpenChange}>
      <SheetContent
        side="left"
        class="sheet-safe w-4/5 max-w-xs gap-3 p-4 pb-[calc(1rem+env(safe-area-inset-bottom))]"
      >
        <SheetHeader>
          <SheetTitle class="text-base">Sessions</SheetTitle>
        </SheetHeader>
        <button type="button" class="new-chat-btn" onClick={props.onNewSession}>
          <FiPlus size="1em" /> New chat
        </button>
        <input
          class="session-search"
          placeholder="Search sessions…"
          value={props.sessionSearch()}
          onInput={(e) => props.setSessionSearch(e.currentTarget.value)}
        />
        <div class="-mx-1 flex-1 overflow-y-auto px-1">
          <For each={props.sessionGroups()}>
            {(grp) => (
              <>
                <div class="session-group-label">{grp.label}</div>
                <For each={grp.items}>
                  {(s) => (
                    <div
                      class={`item-card session-item ${s.id === props.currentSession() ? "active" : ""}`}
                      role="option"
                      tabIndex={0}
                      onClick={() => props.onSwitchSession(s.id)}
                      onKeyDown={activateOnKey(() =>
                        props.onSwitchSession(s.id),
                      )}
                    >
                      <div class="item-body">
                        <div class="item-title">
                          {s.lastMessage || s.id.slice(0, 8)}
                        </div>
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
                          void props.onDeleteSession(s.id);
                        }}
                        onKeyDown={(e) => e.stopPropagation()}
                      >
                        <FiTrash2 size="0.95em" />
                      </button>
                    </div>
                  )}
                </For>
              </>
            )}
          </For>
          <Show when={!props.sessionGroups().length}>
            <div class="empty-note">no sessions match</div>
          </Show>
        </div>

        <div class="mt-auto">
          {/* D15 预览入口。放在抽屉里而非顶栏：顶栏刚被精简过（去掉了设置
              齿轮），不再往上堆图标。 */}
          <button
            type="button"
            class="settings-row"
            onClick={() => {
              props.onOpenChange(false);
              props.onOpenPreview();
            }}
          >
            <span class="settings-icon-chip">
              <FiEye size="1.05em" />
            </span>
            <div class="settings-row-body">
              <div class="settings-row-title">Preview</div>
              <div class="settings-row-sub">
                render HTML/CSS/JS from the workspace
              </div>
            </div>
            <span class="settings-chevron">
              <FiChevronRight size="1em" />
            </span>
          </button>
          <button
            type="button"
            class="settings-row"
            onClick={props.onOpenSettings}
          >
            <span class="settings-icon-chip">
              <FiSettings size="1.05em" />
            </span>
            <div class="settings-row-body">
              <div class="settings-row-title">Settings</div>
              <div class="settings-row-sub">
                AI model · MCP servers · Skills
              </div>
            </div>
            <span class="settings-chevron">
              <FiChevronRight size="1em" />
            </span>
          </button>
        </div>
      </SheetContent>
    </Sheet>
  );
}
