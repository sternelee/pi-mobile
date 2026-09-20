//! pi-ai 模型目录 —— **Rust 侧的单一真源**（数据由 `scripts/gen-models-catalog.py`
//! 从 `@earendil-works/pi-ai/dist/providers/data/*.json` 生成，升级 pi-ai 后重跑）。
//!
//! 为什么目录在 Rust 而不是 bundle：换掉 bun 之后，UI 的 provider/模型列表不能再由
//! pi-ai（JS）提供，而目录本身是 551KB 纯数据 —— 塞进 qjs bundle 会把 385KB 的产物
//! 翻倍；放 Rust 一份，**事件（providers_listed / models_listed）与传输层
//! （baseUrl / compat / thinkingLevelMap）两处共用**，不会出现第二份抄本。
//!
//! 与 bun 路线的两处有意差异（都会体现在 UI 上，所以写清楚）：
//!  1. **不发网络请求**。bun 的 `__pi_models_refresh` 对动态目录（openrouter 等）
//!     会 `force:true` 真拉一次；这里只读静态目录 —— 离线可用，代价是目录随
//!     pi-ai 版本而非线上变化。
//!  2. **模型按 id 排序**。`serde_json::Value` 的 map 是 BTreeMap（没开
//!     `preserve_order`），所以目录里的插入顺序（pi-ai 的新旧序）丢了。没有为这点
//!     排序去开全局 `preserve_order` —— 那会连带改掉会话/registry 落盘的键序。

use std::sync::OnceLock;

use serde_json::{json, Map, Value};

const CATALOG_JSON: &str = include_str!("../../assets/models.json");

/// UI 的 provider 清单 —— 与 `src/lib/providers.tsx` 的 `UI_PROVIDERS` **同序**。
///
/// `(UI id, 目录里的 provider id, 展示名)`
///
/// 展示名为什么以 UI 为准：`providers_listed` 会**整份替换** UI 的静态列表，
/// 直接用目录里的 provider 名会让用户看到 "Anthropic API key" / "Google" 这类
/// 内部命名，与他在设置页里点开前看到的名字不一致。目录只供 id 与模型，名字归产品。
///
/// `google-gemini` → `google` 是唯一的别名：UI 的 id 是历史命名，pi-ai 里叫
/// `google`（bun 路线其实没做这个映射 —— `models.getProvider("google-gemini")`
/// 取不到，那一行一直只有 provider 名没有模型）。
pub const UI_PROVIDERS: [(&str, &str, &str); 8] = [
    ("openai", "openai", "OpenAI"),
    ("openrouter", "openrouter", "OpenRouter"),
    ("deepseek", "deepseek", "DeepSeek"),
    ("google-gemini", "google", "Google Gemini"),
    ("anthropic", "anthropic", "Anthropic (Claude Pro/Max)"),
    ("openai-codex", "openai-codex", "OpenAI Codex (ChatGPT)"),
    ("kimi-coding", "kimi-coding", "Kimi For Coding"),
    ("xai", "xai", "xAI (SuperGrok/X Premium)"),
];

/// 回退模型：provider.json 缺失/坏掉时用（也是 qjs 传输目前唯一能真发的目录项）。
const FALLBACK_PROVIDER: &str = "deepseek";
const FALLBACK_MODEL_ID: &str = "deepseek-v4-flash";

/// 空目录项的落点 —— 让「查不到」也能返回 `&'static Value` 而不是 Option 套 Option。
static NULL: Value = Value::Null;

/// 传输家族 —— **一个家族实现一次，就解锁所有用它的 provider**。
///
/// 换引擎的真实代价就在这张表：pi-ai 的传输层绑 4 家厂商 SDK + node 内建，QuickJS
/// 里跑不了，只能一家族一家族用 Rust 重写。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Transport {
    /// OpenAI Chat Completions 线格式（目录里 653 模型 / 26 provider）。
    /// ⚠️ 目前只实现了 **DeepSeek 的 compat 档案**（max_tokens / system 角色 /
    /// thinkingFormat=deepseek）；泛化到整族见 docs/PROGRESS.md。
    OpenAiCompletions,
    /// OpenAI Responses 线格式（105 模型 / 6 provider，含 UI 第一行 openai）。
    OpenAiResponses,
}

/// 按**模型**（`api` + `provider`）判定走哪条传输。
///
/// 判定依据是 **api 家族而不是 provider 名**：这一层换成家族之后，再加一家 provider
/// 只是目录里多一行，不用再动代码。
///
/// 返回 `Err` 的两种情况要分清，它们对用户的含义不同：
///  · 这一族压根没实现（anthropic-messages / google-* / bedrock …）；
///  · 这一族实现了，但这家 provider 有我们没做的特殊要求（如 github-copilot 的
///    动态头）—— 宁可说清楚，也不发一个必失败的请求。
pub fn transport_for(model: &Value) -> Result<Transport, String> {
    let api = model["api"].as_str().unwrap_or_default();
    let provider = model["provider"].as_str().unwrap_or_default();
    match api {
        "openai-responses" => {
            // github-copilot 要一套动态头（Copilot-Vision-Request / Editor-Version…）
            // 且不用 api key 认证，没做就不能假装支持。
            if provider == "github-copilot" {
                return Err(
                    "github-copilot 需要动态请求头与 OAuth，qjs 未实现 —— 换一家 provider"
                        .to_string(),
                );
            }
            Ok(Transport::OpenAiResponses)
        }
        // 这一族已实现，但只做了 DeepSeek 的 compat 档案（别的 provider 的
        // maxTokensField / thinkingFormat / developer role 都不一样）
        "openai-completions" if provider == "deepseek" => Ok(Transport::OpenAiCompletions),
        "openai-completions" => Err(format!(
            "qjs 的 openai-completions 目前只实现了 DeepSeek 的 compat 档案（{provider} 还没接）\
—— 泛化这一族见 docs/PROGRESS.md"
        )),
        other => Err(format!(
            "qjs 暂无 `{other}` 这一族的模型传输（已实现：openai-responses 全家 + \
openai-completions 的 DeepSeek）—— 换一家 provider，或见 docs/PROGRESS.md"
        )),
    }
}

/// `(UI id, 目录 provider id, 展示名)`；不认识的 id 返回 `None`。
pub fn ui_provider(ui_id: &str) -> Option<(&'static str, &'static str, &'static str)> {
    UI_PROVIDERS.iter().copied().find(|(id, _, _)| *id == ui_id)
}

fn catalog() -> &'static Value {
    static PARSED: OnceLock<Value> = OnceLock::new();
    PARSED.get_or_init(|| serde_json::from_str(CATALOG_JSON).unwrap_or(Value::Null))
}

/// `google-gemini` / `google` 都收 —— 调用方可能给 UI id（事件面）或目录 id（传输面）。
fn to_catalog_id(id: &str) -> &str {
    match ui_provider(id) {
        Some((_, catalog_id, _)) => catalog_id,
        None => id,
    }
}

/// 目录 provider id → UI id。
///
/// 为什么传输层也要它：**凭证是按 UI id 存的**（UI 的 `set_creds` 用的就是它），
/// 而请求体里模型的 `provider` 是目录 id。两家同名时（deepseek）看不出差别，
/// `google`/`google-gemini` 这类不同名的就会查不到 key —— 所以这一层现在就要在。
pub fn to_ui_id(catalog_id: &str) -> &str {
    UI_PROVIDERS
        .iter()
        .find(|(_, cat, _)| *cat == catalog_id)
        .map(|(ui, _, _)| *ui)
        .unwrap_or(catalog_id)
}

fn models_of(id: &str) -> Option<&'static Map<String, Value>> {
    catalog()
        .get("providers")?
        .get(to_catalog_id(id))?
        .as_object()
}

/// `[{ id, name }]` —— UI 的模型列表 / 选择器要的形状。
pub fn model_list(provider: &str) -> Result<Value, String> {
    let models = models_of(provider).ok_or_else(|| format!("unknown provider: {provider}"))?;
    Ok(Value::Array(
        models
            .values()
            .map(|m| {
                let mid = m["id"].as_str().unwrap_or_default();
                json!({ "id": mid, "name": m["name"].as_str().unwrap_or(mid) })
            })
            .collect(),
    ))
}

/// `[{ id, name, models: [...] }]` —— `providers_listed` 的载荷。
///
/// 清单本身是常量（UI 只认这 8 家：图标、OAuth 集合都按它写死），所以即使某家没有
/// 模型也照样发 —— 少发一行会让 UI 的列表凭空少一格。
pub fn providers_list() -> Value {
    Value::Array(
        UI_PROVIDERS
            .iter()
            .map(|(ui_id, cat_id, name)| {
                let models = model_list(cat_id).unwrap_or_else(|_| json!([]));
                json!({ "id": ui_id, "name": name, "models": models })
            })
            .collect(),
    )
}

/// 目录里的完整模型对象（含 baseUrl / compat / thinkingLevelMap —— 传输层要的字段）。
pub fn find_model(provider: &str, model_id: &str) -> Option<&'static Value> {
    models_of(provider)?.get(model_id)
}

/// provider.json 的 `(provider, modelId)` → `(UI id, 完整模型对象)`。
///
/// **有意不在这里回退到 DeepSeek**：用户存的是 openai，就让 model 对象如实是
/// openai —— 传输层会在发请求时明确拒绝（`startModel`）。静默换一家去发，等于让
/// 「UI 显示 OpenAI、真实请求打 DeepSeek」这种最难查的不一致活下来。
/// 只有 provider.json 缺失/坏掉（用户还没选过）才用回退模型。
pub fn resolve_for_boot(
    provider: Option<&str>,
    model_id: Option<&str>,
) -> (String, &'static Value) {
    let Some(provider) = provider.filter(|p| !p.is_empty()) else {
        return (FALLBACK_PROVIDER.to_string(), fallback_model());
    };
    let ui_id = match ui_provider(provider) {
        Some((id, _, _)) => id.to_string(),
        None => provider.to_string(),
    };
    let model = model_id
        .and_then(|id| find_model(provider, id))
        // 目录里没有这个 id（pi-ai 升级删过模型）：退回该 provider 的第一个模型，
        // 而不是让 boot 失败 —— 用户仍能在 UI 里重选。
        .or_else(|| models_of(provider).and_then(|m| m.values().next()))
        .unwrap_or_else(|| fallback_model());
    (ui_id, model)
}

fn fallback_model() -> &'static Value {
    find_model(FALLBACK_PROVIDER, FALLBACK_MODEL_ID).unwrap_or(&NULL)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_list_matches_ui_contract() {
        let list = providers_list();
        let items = list.as_array().unwrap();
        assert_eq!(
            items.len(),
            UI_PROVIDERS.len(),
            "少发/多发都会让 UI 列表错位"
        );
        let ids: Vec<&str> = items.iter().filter_map(|p| p["id"].as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "openai",
                "openrouter",
                "deepseek",
                "google-gemini",
                "anthropic",
                "openai-codex",
                "kimi-coding",
                "xai"
            ]
        );
        for item in items {
            assert!(
                item["name"].as_str().is_some_and(|n| !n.is_empty()),
                "{item}"
            );
            assert!(item["models"].is_array(), "{item}");
        }
        // 展示名以 UI 为准（不是目录里的 "Anthropic API key"）
        let anthropic = items.iter().find(|p| p["id"] == "anthropic").unwrap();
        assert_eq!(anthropic["name"], "Anthropic (Claude Pro/Max)");
    }

    #[test]
    fn google_gemini_alias_resolves_to_catalog_google() {
        // UI id 与目录 id 不同名的那一家：两条 id 都要查到同一份模型
        let by_ui = model_list("google-gemini").unwrap();
        let by_catalog = model_list("google").unwrap();
        assert_eq!(by_ui, by_catalog);
        assert!(
            !by_ui.as_array().unwrap().is_empty(),
            "别名没生效就会是空列表"
        );
        // 事件面回的是 UI id（UI 用自己列表里的 id 匹配），所以别名只在查表时用
        let item = providers_list()
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "google-gemini")
            .cloned()
            .unwrap();
        assert!(!item["models"].as_array().unwrap().is_empty());
    }

    #[test]
    fn model_list_carries_id_and_name_and_rejects_unknown_provider() {
        let models = model_list("deepseek").unwrap();
        let models = models.as_array().unwrap();
        assert!(!models.is_empty());
        for m in models {
            assert!(m["id"].as_str().is_some_and(|s| !s.is_empty()), "{m}");
            assert!(m["name"].as_str().is_some_and(|s| !s.is_empty()), "{m}");
        }
        assert!(model_list("nope").is_err());
    }

    #[test]
    fn find_model_keeps_transport_fields() {
        let model = find_model("deepseek", "deepseek-v4-pro").expect("catalog entry");
        // 传输层真正要用的字段：一个都不能在生成目录时被裁掉
        assert_eq!(model["id"], "deepseek-v4-pro");
        assert_eq!(model["provider"], "deepseek");
        assert_eq!(model["baseUrl"], "https://api.deepseek.com");
        assert_eq!(model["compat"]["thinkingFormat"], "deepseek");
        assert_eq!(model["compat"]["maxTokensField"], "max_tokens");
        assert!(model["thinkingLevelMap"]["high"].is_string());
        assert!(model["contextWindow"].as_u64().is_some_and(|n| n > 0));
        assert!(find_model("deepseek", "no-such-model").is_none());
    }

    #[test]
    fn boot_resolution_prefers_truth_over_fallback() {
        // 有 provider 就如实给那一家的模型（哪怕还没有传输）——
        // 静默换成 DeepSeek 会让 UI 显示与真实请求不一致
        let (ui_id, model) = resolve_for_boot(Some("openai"), Some("gpt-4"));
        assert_eq!(ui_id, "openai");
        assert_eq!(model["provider"], "openai");
        // gpt-4 是 openai-responses → 这轮之后真的能发了
        assert_eq!(transport_for(model), Ok(Transport::OpenAiResponses));

        // 缺 provider.json（用户还没选过）才回退
        let (ui_id, model) = resolve_for_boot(None, None);
        assert_eq!(ui_id, "deepseek");
        assert_eq!(model["id"], FALLBACK_MODEL_ID);
        assert_eq!(transport_for(model), Ok(Transport::OpenAiCompletions));

        // 目录里没有的 modelId（pi-ai 删过模型）→ 退回该 provider 的第一个模型
        let (_, model) = resolve_for_boot(Some("deepseek"), Some("deepseek-v0-gone"));
        assert_eq!(model["provider"], "deepseek");
    }

    #[test]
    fn transport_is_decided_by_api_family_not_provider_name() {
        // responses 家族：整族通（目录里 6 家），只有要动态头的 copilot 除外
        assert_eq!(
            transport_for(&json!({ "api": "openai-responses", "provider": "openai" })),
            Ok(Transport::OpenAiResponses)
        );
        assert_eq!(
            transport_for(&json!({ "api": "openai-responses", "provider": "opencode" })),
            Ok(Transport::OpenAiResponses)
        );
        assert!(
            transport_for(&json!({ "api": "openai-responses", "provider": "github-copilot" }))
                .is_err()
        );

        // completions 家族：只认 DeepSeek（泛化是下一步），其余的表述要能区分
        assert_eq!(
            transport_for(&json!({ "api": "openai-completions", "provider": "deepseek" })),
            Ok(Transport::OpenAiCompletions)
        );
        let err = transport_for(&json!({ "api": "openai-completions", "provider": "openrouter" }))
            .unwrap_err();
        assert!(err.contains("DeepSeek 的 compat"), "{err}");

        // 未实现的家族：错误里要点名是哪一族，用户才知道该怎么办
        let err = transport_for(&json!({ "api": "anthropic-messages", "provider": "anthropic" }))
            .unwrap_err();
        assert!(err.contains("anthropic-messages"), "{err}");
    }

    #[test]
    fn catalog_models_carry_the_api_family_the_transport_dispatches_on() {
        // 目录里每个模型都必须有 api —— 没有它 startModel 只能猜
        for (provider, models) in catalog()["providers"].as_object().unwrap() {
            for (id, model) in models.as_object().unwrap() {
                assert!(
                    model["api"].as_str().is_some_and(|api| !api.is_empty()),
                    "{provider}/{id} 缺 api"
                );
            }
        }
        // UI 那 8 行里，responses 家族覆盖 openai + xai（这轮新增的能力）
        for id in ["gpt-5", "gpt-4o", "grok-4.5"] {
            let model = find_model("openai", id)
                .or_else(|| find_model("xai", id))
                .unwrap();
            assert_eq!(transport_for(model), Ok(Transport::OpenAiResponses), "{id}");
        }
    }

    #[test]
    fn catalog_id_maps_back_to_the_ui_id_that_credentials_use() {
        // 凭证按 UI id 存（UI 的 set_creds），请求体里的 provider 是目录 id
        assert_eq!(to_ui_id("google"), "google-gemini");
        assert_eq!(to_ui_id("deepseek"), "deepseek");
        // 不在 UI 清单里的（目录有 39 家，UI 只认 8 家）原样返回
        assert_eq!(to_ui_id("groq"), "groq");
    }
}
