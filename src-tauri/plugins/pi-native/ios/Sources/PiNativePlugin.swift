// PiNativePlugin.swift —— pi-mobile 自建设备能力的 iOS 侧。
//
// 当前只有定位占位（见下），日历/通讯录/照片（1b）会加在这里。
//
// ## 为什么 iOS 的 location 命令是「明确拒绝」而不是实现
//
// iOS 定位已经由官方 `tauri-plugin-geolocation`（CoreLocation）承担，实测
// 可用（真机精度 ~11m）。本插件只在 Android 上替代它 —— 因为官方 Android
// 实现走 Google fused provider 且没有超时，国内 ROM 上会永久挂起
// （详见 android/src/main/java/com/sternelee/pinative/PiNativePlugin.kt 头注）。
//
// 但命令仍必须在这里存在：`build.rs` 的 COMMANDS 是跨平台共享的清单，
// Rust 侧 `run_mobile_plugin("location")` 的字符串也是跨平台一致的。
// 反注册一个「声明了却不实现」的命令，不如让它被调用时给出可执行的指引 ——
// 静默成功或含糊报错都最难排。上层 `native::location()` 在 iOS 上不会走到
// 这里（它按平台分流到官方插件）。

import SwiftRs
import Tauri
import UIKit
import WebKit

class LocationArgs: Decodable {
  var highAccuracy: Bool?
  var timeoutMs: UInt64?
}

class PermissionArgs: Decodable {
  /// "calendar"（目前唯一需要主动请求的）
  let kind: String
}

class PiNativePlugin: Plugin {
  @objc public func location(_ invoke: Invoke) throws {
    invoke.reject(
      "pi-native.location is Android-only; on iOS call tauri-plugin-geolocation "
        + "(this rejection exists so the command set stays symmetric and a wrong "
        + "routing shows up as a clear error instead of a hang)"
    )
  }

  /// 系统日历读/写（EventKit）。实现见 Calendar.swift。
  @objc public func calendar(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(CalendarArgs.self)
    CalendarBridge.handle(args, invoke)
  }

  /// 查询权限当前状态（同步、不弹窗）—— 设置页展示用。
  @objc public func permissionState(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(PermissionArgs.self)
    switch args.kind {
    case "calendar":
      invoke.resolve(["kind": "calendar", "state": CalendarAccess.currentState()])
    default:
      invoke.reject("unknown permission kind '\(args.kind)'")
    }
  }

  /// 主动请求系统权限（弹窗）。目前只有日历 —— 定位/通知/剪贴板的授权
  /// 走各自的官方插件。
  @objc public func requestPermission(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(PermissionArgs.self)
    switch args.kind {
    case "calendar":
      CalendarAccess.requestFullAccess(invoke)
    default:
      invoke.reject("unknown permission kind '\(args.kind)'")
    }
  }
}

@_cdecl("init_plugin_pi_native")
func initPlugin() -> Plugin {
  return PiNativePlugin()
}
