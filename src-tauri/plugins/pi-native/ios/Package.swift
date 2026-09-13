// swift-tools-version:5.3
// pi-native 的 iOS 侧 Swift 包。
//
// 依赖 ../.tauri/tauri-api —— 那个目录由 tauri-plugin 的 build.rs 从
// tauri 的 iOS 库拷进来（见 plugins/pi-native/build.rs 与
// docs/PROGRESS.md 里关于 iOS 插件注入机制的侦察结论）。
import PackageDescription

let package = Package(
  name: "tauri-plugin-pi-native",
  platforms: [
    .macOS(.v10_13),
    .iOS(.v14),
  ],
  products: [
    .library(
      name: "tauri-plugin-pi-native",
      type: .static,
      targets: ["tauri-plugin-pi-native"])
  ],
  dependencies: [
    .package(name: "Tauri", path: "../.tauri/tauri-api")
  ],
  targets: [
    .target(
      name: "tauri-plugin-pi-native",
      dependencies: [
        .byName(name: "Tauri")
      ],
      path: "Sources")
  ]
)
