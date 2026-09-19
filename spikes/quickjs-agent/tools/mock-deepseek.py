#!/usr/bin/env python3
"""DeepSeek 的 mock：用 OpenAI Chat Completions 的 SSE 格式回放一段脚本化的对话。

存在的理由：整条链（prelude → bundle → boot → prompt → streamFn → poll →
工具往返 → 第二轮请求）都能在没有 API key、不花钱、不出网的情况下验证。
spike 里 `DEEPSEEK_BASE_URL` 就是为它留的。

脚本化行为（**无状态**，只看这次请求带没带工具结果，因此可重复跑）：
  消息里没有 role:"tool" → 回一个 `read` 工具调用（arguments 分两片发，验证流式拼接）
  消息里已有 role:"tool" → 回一段收尾文本（thinking + text 两类增量）

用法：python3 tools/mock-deepseek.py [port] [host]   # 默认 8899 / 127.0.0.1
     真机连宿主时用 host=0.0.0.0（否则只监听回环，设备连不上）
"""

import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8899
HOST = sys.argv[2] if len(sys.argv) > 2 else "127.0.0.1"
STATE = {"requests": 0, "bodies": []}


def chunk(delta, finish=None, usage=None):
    payload = {
        "id": "chatcmpl-mock",
        "object": "chat.completion.chunk",
        "model": "deepseek-v4-flash",
        "choices": [] if usage else [{"index": 0, "delta": delta, "finish_reason": finish}],
    }
    if usage:
        payload["usage"] = usage
    return f"data: {json.dumps(payload)}\n\n"


def emit(write, pieces, finish, usage):
    """发一串增量，然后 finish + usage + [DONE]。

    注意：只在**完整**响应里用它。工具调用那一轮要自己写 chunk 顺序 —— 第一次
    实现就是把 [DONE] 提前发了，宿主解析器读到 [DONE] 直接收工，工具调用整段丢失。
    """
    for piece in pieces:
        write(chunk(piece).encode())
        time.sleep(0.01)  # 让增量真的分片到达，而不是一次性到齐
    write(chunk({}, finish=finish).encode())
    write(chunk({}, usage=usage).encode())
    write(b"data: [DONE]\n\n")


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # 静音默认 access log
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        body = json.loads(self.rfile.read(length) or b"{}")
        STATE["requests"] += 1
        index = STATE["requests"]
        STATE["bodies"].append(body)
        has_tool_result = any(m.get("role") == "tool" for m in body.get("messages", []))
        print(
            f"[mock] request #{index}: model={body.get('model')} "
            f"messages={len(body.get('messages', []))} tools={len(body.get('tools', []))} "
            f"thinking={body.get('thinking')} effort={body.get('reasoning_effort')} "
            f"tool_result={'yes' if has_tool_result else 'no'}",
            flush=True,
        )

        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()

        # 固定值：让重复跑得到完全一样的数字（含 DeepSeek 特有的 prompt_cache_hit_tokens）
        usage = {
            "prompt_tokens": 900,
            "completion_tokens": 40,
            "prompt_cache_hit_tokens": 800,
            "completion_tokens_details": {"reasoning_tokens": 12},
        }

        if not has_tool_result:
            # 第一轮：思考增量 → 工具调用（arguments 刻意分两片）→ finish → usage → [DONE]
            for piece in (
                {"reasoning_content": "I should look at the workspace. "},
                {"reasoning_content": "Start with notes.md."},
            ):
                self.wfile.write(chunk(piece).encode())
                time.sleep(0.01)
            self.wfile.write(
                chunk(
                    {
                        "tool_calls": [
                            {
                                "index": 0,
                                "id": "call_mock_1",
                                "type": "function",
                                "function": {"name": "read", "arguments": '{"path"'},
                            }
                        ]
                    }
                ).encode()
            )
            time.sleep(0.05)
            self.wfile.write(
                chunk(
                    {
                        "tool_calls": [
                            {"index": 0, "function": {"arguments": ': "notes.md"}'}}
                        ]
                    }
                ).encode()
            )
            self.wfile.write(chunk({}, finish="tool_calls").encode())
            self.wfile.write(chunk({}, usage=usage).encode())
            self.wfile.write(b"data: [DONE]\n\n")
            return

        emit(
            self.wfile.write,
            [
                {"reasoning_content": "The tool returned the file body. "},
                {"content": "Done. "},
                {"content": "I read notes.md and the workspace looks like a spike sandbox."},
            ],
            finish="stop",
            usage=usage,
        )

    def do_GET(self):
        if self.path == "/__state":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            body = json.dumps(
                {"requests": STATE["requests"], "last_body": STATE["bodies"][-1] if STATE["bodies"] else None}
            ).encode()
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_response(404)
        self.end_headers()


if __name__ == "__main__":
    print(f"[mock] DeepSeek SSE mock listening on http://{HOST}:{PORT}", flush=True)
    ThreadingHTTPServer((HOST, PORT), Handler).serve_forever()
