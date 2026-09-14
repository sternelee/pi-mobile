// 把真实的 src-tauri/src/script.rs 复制进 OUT_DIR，供 main.rs `include!`。
//
// 为什么复制而不是直接 include 源文件：script.rs 开头是 `//!` 内部文档注释，
// 而 `include!` 的展开位置不允许内部文档注释（E0753）。这里**只**把行首的
// `//!` 换成 `//`（纯文档标记，语义零变化）。
//
// 为什么用 build.rs 生成而不是在仓里放一份副本：副本会漂移 —— 而这份 harness
// 的全部价值就在于「判定的就是产品里那一份策略」。生成保证了永不漂移；脚本
// 结尾还会打印两文件的 sha256 供核对。
use std::env;
use std::fs;
use std::path::Path;

fn main() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../src-tauri/src/script.rs")
        .canonicalize()
        .expect("找不到 src-tauri/src/script.rs");
    println!("cargo:rerun-if-changed={}", src.display());

    let body = fs::read_to_string(&src).expect("读 script.rs 失败");
    let stripped: String = body
        .lines()
        .map(|l| match l.strip_prefix("//!") {
            Some(rest) => format!("//{rest}"),
            None => l.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");

    let out = Path::new(&env::var("OUT_DIR").unwrap()).join("script_included.rs");
    fs::write(&out, stripped).expect("写 script_included.rs 失败");
    println!("cargo:rustc-env=D14_SCRIPT_RS={}", src.display());
}
