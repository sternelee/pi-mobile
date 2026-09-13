// pi-native 的构建脚本 —— 与官方插件同款机制。
//
// `tauri_plugin::Builder::try_build()` 在移动端做两件事（见
// tauri-plugin/src/build/mobile.rs）：
//   * iOS（宿主为 macOS 交叉编译时）：`link_apple_library(name, ios/)` 把
//     ios/ 下的 Swift 包**编成静态库直接链进 Rust 产物**，并把 tauri-api
//     拷进 ios/.tauri/。所以 Swift 符号出现在 libapp.a 里，Xcode 工程
//     （gen/apple/project.pbxproj）保持干净 —— 无需手改工程，与「改完
//     project.yml 要跑 xcodegen generate」的现有流程也不冲突。
//   * Android：把 android/ 路径以 `cargo:android_library_path=` 交给 gradle。
//
// COMMANDS 是 Rust ↔ 原生两侧的方法名清单，必须与 src/mobile.rs 里
// run_mobile_plugin 的字符串、以及两侧原生实现的命令名三处一致。
const COMMANDS: &[&str] = &[
    "location",
    "calendar",
    "permissionState",
    "requestPermission",
];

fn main() {
    tauri_plugin::Builder::new(COMMANDS)
        .ios_path("ios")
        .android_path("android")
        .try_build()
        .expect("pi-native: tauri_plugin build failed");
}
