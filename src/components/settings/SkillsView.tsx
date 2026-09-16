import { For, Show } from "solid-js";
import { Button } from "~/components/ui/button";
import { ToggleSwitch } from "~/components/ui/switch";
import type { SkillMeta } from "~/lib/types";

interface Props {
  skills: () => SkillMeta[];
  onRemove: (id: string) => void;
  onToggle: (id: string, enabled: boolean) => void;
  skillUrl: () => string;
  setSkillUrl: (v: string) => void;
  installingSkill: () => boolean;
  installSkill: (e: Event) => void;
}

export function SkillsView(props: Props) {
  return (
    <>
      <div class="settings-section-title">Skills</div>
      <div class="settings-subtitle">
        SKILL.md packages whose instructions are injected into the system prompt
        — no code runs on this device.
      </div>
      <div class="-mx-1 flex-1 overflow-y-auto px-1">
        <For each={props.skills()}>
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
                  onClick={() => props.onRemove(s.id)}
                >
                  Remove
                </Button>
              </div>
              <ToggleSwitch
                on={s.enabled}
                onChange={() => props.onToggle(s.id, !s.enabled)}
              />
            </div>
          )}
        </For>
        <Show when={!props.skills().length}>
          <div class="empty-note">no skills installed</div>
        </Show>
        <form onSubmit={props.installSkill} class="mt-2 flex flex-col gap-1.5">
          <input
            class="ask-input"
            placeholder="https://github.com/owner/repo or SKILL.md URL"
            value={props.skillUrl()}
            onInput={(e) => props.setSkillUrl(e.currentTarget.value)}
          />
          <Button
            variant="outline"
            size="sm"
            type="submit"
            disabled={props.installingSkill()}
          >
            {props.installingSkill() ? "Installing…" : "Install skill"}
          </Button>
        </form>
        <div class="item-sub mt-1">
          SKILL.md instructions inject into the system prompt · disabled = not
          injected · no code runs
        </div>
      </div>
    </>
  );
}
