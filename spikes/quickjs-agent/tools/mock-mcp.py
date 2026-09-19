#!/usr/bin/env python3
"""MCP streamable-http 的 mock 服务器。

存在的理由与 mock-deepseek.py 相同：整条 MCP 链路（连接 → tools/list → 工具注册 →
tools/call）要在**不出网、不依赖外部服务器**的情况下可验证。

两种应答模式都要覆盖，因为它们暴露的是不同代码路径：
  --json（默认）  application/json 一次给全 —— 走 `host.http` 的默认缓冲模式
  --sse          text/event-stream —— 响应体挂着不关，必须靠 `readMode: "first-event"`
                 才不会被 30s 超时拖死（MCP 规范允许长连接 SSE）

用法：python3 tools/mock-mcp.py [port] [host] [--sse]
"""

import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ARGS = [a for a in sys.argv[1:] if not a.startswith("--")]
SSE = "--sse" in sys.argv
PORT = int(ARGS[0]) if len(ARGS) > 0 else 8901
HOST = ARGS[1] if len(ARGS) > 1 else "127.0.0.1"

SESSION_ID = "mock-session-1"
TOOLS = [
    {
        "name": "echo",
        "description": "Echo back the given text (mock server).",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string", "description": "Text to echo"}},
            "required": ["text"],
        },
    },
    {
        "name": "add",
        "description": "Add two integers (mock server).",
        "inputSchema": {
            "type": "object",
            "properties": {"a": {"type": "integer"}, "b": {"type": "integer"}},
            "required": ["a", "b"],
        },
    },
]
STATE = {"requests": 0, "calls": 0}


def result_for(method, params):
    if method == "initialize":
        return {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "mock-mcp", "version": "0.1.0"},
        }
    if method == "tools/list":
        return {"tools": TOOLS}
    if method == "tools/call":
        STATE["calls"] += 1
        name, args = params.get("name"), params.get("arguments") or {}
        if name == "echo":
            return {"content": [{"type": "text", "text": f"echo: {args.get('text', '')}"}], "isError": False}
        if name == "add":
            try:
                total = int(args.get("a", 0)) + int(args.get("b", 0))
                return {"content": [{"type": "text", "text": str(total)}], "isError": False}
            except (TypeError, ValueError):
                return {"content": [{"type": "text", "text": "a and b must be integers"}], "isError": True}
        return {"content": [{"type": "text", "text": f"unknown tool {name}"}], "isError": True}
    return None


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_POST(self):
        length = int(self.headers.get("Content-Length", "0"))
        raw = self.rfile.read(length) or b"{}"
        try:
            request = json.loads(raw)
        except json.JSONDecodeError:
            self.send_response(400)
            self.end_headers()
            return
        STATE["requests"] += 1
        method = request.get("method", "")
        rid = request.get("id")
        print(
            f"[mock-mcp] #{STATE['requests']} {method} id={rid} "
            f"session={self.headers.get('mcp-session-id')} accept={self.headers.get('accept')}",
            flush=True,
        )

        # 通知（无 id）：202 + 空体，MCP 规范如此
        if rid is None:
            self.send_response(202)
            self.send_header("mcp-session-id", SESSION_ID)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return

        result = result_for(method, request.get("params") or {})
        message = (
            {"jsonrpc": "2.0", "id": rid, "result": result}
            if result is not None
            else {"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": f"no such method: {method}"}}
        )

        if SSE:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("mcp-session-id", SESSION_ID)
            self.send_header("Connection", "close")
            self.end_headers()
            # 先发一条**无关通知**（客户端必须跳过它去找匹配 id 的那条），
            # 再发真正的响应 —— 这正是 readSseResponse 要处理的场景。
            self.wfile.write(b'event: message\ndata: {"jsonrpc":"2.0","method":"notifications/progress"}\n\n')
            self.wfile.flush()
            time.sleep(0.05)
            self.wfile.write(f"event: message\ndata: {json.dumps(message)}\n\n".encode())
            self.wfile.flush()
            # 故意**不关连接**一会儿：默认缓冲模式会在这里挂到超时，
            # readMode=first-event 则读完第一个事件就返回。
            if method == "tools/call":
                time.sleep(3)
            return

        body = json.dumps(message).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("mcp-session-id", SESSION_ID)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path == "/__state":
            body = json.dumps({"requests": STATE["requests"], "calls": STATE["calls"], "sse": SSE}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_response(404)
        self.end_headers()


if __name__ == "__main__":
    mode = "SSE（长连接，需 first-event 读取）" if SSE else "JSON"
    print(f"[mock-mcp] listening on http://{HOST}:{PORT}  mode={mode}", flush=True)
    ThreadingHTTPServer((HOST, PORT), Handler).serve_forever()
