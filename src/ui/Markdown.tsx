// 轻量 Markdown 渲染（零依赖）：覆盖 LLM 输出的绝大多数形态 ——
// 围栏代码块（语言标签 + 复制）、标题、有序/无序列表、引用、粗斜体、
// 行内代码、链接。先 HTML 转义再做自有转换，无注入面。
import { For, Show, createSignal } from "solid-js";

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function inline(s: string): string {
  let h = escapeHtml(s);
  h = h.replace(/`([^`]+)`/g, "<code>$1</code>");
  h = h.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>");
  h = h.replace(/(^|[^*\w])\*([^*\n]+)\*(?!\w)/g, "$1<em>$2</em>");
  h = h.replace(
    /\[([^\]]+)\]\((https?:[^)\s]+)\)/g,
    '<a href="$2" target="_blank" rel="noopener noreferrer">$1</a>',
  );
  return h;
}

type Block =
  | { type: "code"; lang: string; code: string }
  | { type: "md"; html: string };

function mdToHtml(lines: string[]): string {
  let html = "";
  let i = 0;
  const isListItem = (l: string) => /^[-*]\s+/.test(l) || /^\d+[.)]\s+/.test(l);
  while (i < lines.length) {
    const line = lines[i];
    if (!line.trim()) {
      i++;
      continue;
    }
    const heading = line.match(/^(#{1,4})\s+(.*)$/);
    if (heading) {
      const level = Math.min(heading[1].length + 2, 5); // ## → h4，聊天里压层级
      html += `<h${level}>${inline(heading[2])}</h${level}>`;
      i++;
      continue;
    }
    if (isListItem(line)) {
      const ordered = /^\d+[.)]\s+/.test(line);
      const items: string[] = [];
      while (i < lines.length && isListItem(lines[i])) {
        items.push(inline(lines[i].replace(/^[-*]\s+/, "").replace(/^\d+[.)]\s+/, "")));
        i++;
      }
      const tag = ordered ? "ol" : "ul";
      html += `<${tag}>${items.map((it) => `<li>${it}</li>`).join("")}</${tag}>`;
      continue;
    }
    if (line.startsWith(">")) {
      const quote: string[] = [];
      while (i < lines.length && lines[i].startsWith(">")) {
        quote.push(inline(lines[i].replace(/^>\s?/, "")));
        i++;
      }
      html += `<blockquote>${quote.join("<br/>")}</blockquote>`;
      continue;
    }
    if (/^(---+|\*\*\*+)$/.test(line.trim())) {
      html += "<hr/>";
      i++;
      continue;
    }
    // 段落：连续非结构行
    const para: string[] = [];
    while (
      i < lines.length &&
      lines[i].trim() &&
      !/^(#{1,4})\s+/.test(lines[i]) &&
      !isListItem(lines[i]) &&
      !lines[i].startsWith(">")
    ) {
      para.push(lines[i]);
      i++;
    }
    html += `<p>${inline(para.join("\n")).replace(/\n/g, "<br/>")}</p>`;
  }
  return html;
}

function parse(text: string): Block[] {
  const lines = text.replace(/\r\n/g, "\n").split("\n");
  const blocks: Block[] = [];
  let md: string[] = [];
  let i = 0;
  const flush = () => {
    if (md.length) {
      blocks.push({ type: "md", html: mdToHtml(md) });
      md = [];
    }
  };
  while (i < lines.length) {
    const fence = lines[i].match(/^```\s*(\S*)\s*$/);
    if (fence) {
      flush();
      const lang = fence[1] ?? "";
      const code: string[] = [];
      i++;
      while (i < lines.length && !/^```\s*$/.test(lines[i])) {
        code.push(lines[i]);
        i++;
      }
      i++; // 跳过收尾 ```
      blocks.push({ type: "code", lang, code: code.join("\n") });
      continue;
    }
    md.push(lines[i]);
    i++;
  }
  flush();
  return blocks;
}

function CodeBlock(props: { lang: string; code: string }) {
  const [copied, setCopied] = createSignal(false);
  const copy = () => {
    navigator.clipboard
      ?.writeText(props.code)
      .then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1200);
      })
      .catch(() => {});
  };
  return (
    <div class="codeblock">
      <div class="codeblock-head">
        <span>{props.lang || "code"}</span>
        <button class="codeblock-copy" onClick={copy}>
          {copied() ? "copied ✓" : "copy"}
        </button>
      </div>
      <pre>
        <code innerHTML={escapeHtml(props.code)} />
      </pre>
    </div>
  );
}

export function Markdown(props: { text: string }) {
  return (
    <div class="md">
      <For each={parse(props.text)}>
        {(b) => (
          <Show
            when={b.type === "code"}
            fallback={<div innerHTML={(b as { type: "md"; html: string }).html} />}
          >
            <CodeBlock
              lang={(b as { type: "code"; lang: string }).lang}
              code={(b as { type: "code"; code: string }).code}
            />
          </Show>
        )}
      </For>
    </div>
  );
}
