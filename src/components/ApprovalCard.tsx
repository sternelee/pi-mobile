import { FiAlertTriangle } from "solid-icons/fi";
import { For, Show } from "solid-js";
import { Button } from "~/components/ui/button";
import type { Approval } from "~/lib/types";

interface Props {
  approval: () => Approval | null;
  capLabel: (id: string) => string;
  onDecide: (decision: "allow" | "deny" | "always") => void;
}

export function ApprovalCard(props: Props) {
  return (
    <Show when={props.approval()}>
      {(a) => (
        <div class="approval">
          <div class="approval-title">
            <FiAlertTriangle
              size="0.95em"
              style={{ "vertical-align": "-0.12em" }}
            />{" "}
            {/* 脚本没有 path，硬拼 «» 会变成 run_js «» —— 与之前修过的
                `approvalTarget` 是同一类文案 bug。脚本走单独标题。 */}
            {a().script
              ? "run a script — approve?"
              : `${a().tool} «${a().path}» — approve?`}
          </div>
          <Show when={(a().capabilities?.length ?? 0) > 0}>
            <div class="cap-list">
              <div class="cap-list-head">This script will be able to:</div>
              <For each={a().capabilities}>
                {(id) => (
                  <div class="cap-row">
                    <span class="cap-dot">•</span>
                    <span class="cap-desc">{props.capLabel(id)}</span>
                    <span class="cap-id">{id}</span>
                  </div>
                )}
              </For>
            </div>
          </Show>
          <Show when={a().script && !(a().capabilities?.length ?? 0)}>
            {/* 空清单必须明说：否则用户会以为「卡上没写就是没风险」 */}
            <div class="cap-list-head">
              This script requests no device or file access.
            </div>
          </Show>
          {/* D14 / §2.2 要求：用户批的是「这份能力清单 + 这段代码」，两者都
              必须可见 —— 否则「批准」就成了一个不知道批了什么的动作。 */}
          <Show when={a().code}>
            <pre class="script-code">{a().code}</pre>
          </Show>
          <Show when={a().diff}>
            <div class="diff">
              {a()
                .diff.split("\n")
                .map((line) => (
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
            <Button
              variant="destructive"
              onClick={() => props.onDecide("deny")}
            >
              Deny
            </Button>
            {/* 脚本不提供 Always：D14 规定脚本的 always 只对本次生效、不降
                全局基线。把一个点了之后不再生效的按钮摆在那里会骗人 ——
                用户会以为「以后这类脚本都行」，而实际每次都会再问。 */}
            <Show when={!a().script}>
              <Button
                variant="secondary"
                onClick={() => props.onDecide("always")}
              >
                Always
              </Button>
            </Show>
            <Button
              class="bg-success text-success-foreground hover:bg-success/90"
              onClick={() => props.onDecide("allow")}
            >
              Allow
            </Button>
          </div>
        </div>
      )}
    </Show>
  );
}
