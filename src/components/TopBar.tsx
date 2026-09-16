import { FiMenu } from "solid-icons/fi";
import { Show } from "solid-js";
import { Button } from "~/components/ui/button";
import { fmtTok } from "~/lib/format";

interface Props {
  title: () => string;
  tokens: () => number;
  onOpenDrawer: () => void;
}

export function TopBar(props: Props) {
  return (
    <header class="topbar">
      <div class="topbar-actions">
        <Button
          variant="secondary"
          size="icon"
          class="h-8 w-8"
          onClick={props.onOpenDrawer}
          aria-label="sessions"
        >
          <FiMenu size="1.05em" />
        </Button>
      </div>
      {/* 标题 = 会话标题；右侧只留 token 用量。
          原来这里还有会话 id 短码与设置齿轮 —— id 对用户无意义、齿轮与
          侧边栏底部的 Settings 入口重复，两者都已移除。 */}
      <h1 class="topbar-title" title={props.title()}>
        {props.title()}
      </h1>
      <div class="topbar-meta">
        <Show when={props.tokens() > 0}>
          <span class="tok-badge">{fmtTok(props.tokens())}</span>
        </Show>
      </div>
    </header>
  );
}
