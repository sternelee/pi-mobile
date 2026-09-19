#!/usr/bin/env python3
"""从 pi-ai 的模型目录生成 App 用的合并目录（src-tauri/assets/models.json）。

为什么要这一步：换掉 bun 之后，UI 的 provider/模型列表不能再由 pi-ai（JS）提供，
但**目录本身是纯数据**（`@earendil-works/pi-ai/dist/providers/data/*.json`，
39 个 provider / 1290 个模型 / 551KB），可以直接搬进 Rust —— 这部分属于
「pi-ai 里能集成的部分」（另外半是协议编解码，已在 qjs 的传输里复刻）。

用法：python3 scripts/gen-models-catalog.py   （升级 pi-ai 后重跑）
"""
import json
import glob
from pathlib import Path

SRC = "node_modules/@earendil-works/pi-ai/dist/providers/data/*.json"
OUT = Path("src-tauri/assets/models.json")

by_provider: dict[str, dict] = {}
api_by_provider: dict[str, str] = {}
for path in sorted(glob.glob(SRC)):
    data = json.loads(Path(path).read_text())
    for api_family, models in data.items():
        for model_id, model in models.items():
            provider = model.get("provider")
            if not provider:
                continue
            by_provider.setdefault(provider, {})[model_id] = model
            api_by_provider.setdefault(provider, api_family)

# 传输层要用的字段全留（api/baseUrl/compat/thinkingLevelMap/cost/contextWindow…），
# UI 只用 id/name —— 一份数据两处用，避免再抄一遍目录。
merged = {
    "apis": api_by_provider,
    "providers": by_provider,
}
OUT.write_text(json.dumps(merged, ensure_ascii=False, separators=(",", ":")))
models = sum(len(m) for m in by_provider.values())
print(f"{OUT}: {len(by_provider)} providers / {models} models / {OUT.stat().st_size/1024:.0f} KB")
