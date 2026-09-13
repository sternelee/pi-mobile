// Calendar.swift —— 系统日历读写（EventKit）。
//
// ## 权限：iOS 17 起必须区分「读」与「写」
//
// iOS 17 把日历权限拆成三档，Info.plist 的键也随之拆分：
//   * `NSCalendarsFullAccessUsageDescription`   —— 完整读写（iOS 17+）
//   * `NSCalendarsWriteOnlyAccessUsageDescription` —— 仅写（iOS 17+）
//   * `NSCalendarsUsageDescription`             —— iOS 16 及以前的老键
// 只声明老键、在 iOS 17+ 上请求 `requestFullAccessToEvents` 会被系统**直接
// 拒绝**（不弹窗）。所以三个键都在 Info.plist 里声明，并按系统版本走对应 API。
//
// ## 时间
//
// 入参/出参一律 epoch 毫秒。EventKit 用 Date，转换集中在本文件，避免
// 各处各自换算时把毫秒/秒或时区搞错。

import EventKit
import Foundation
import Tauri

/// `calendar` 命令的参数（字段名与 Rust 侧 CalendarArgs 的 camelCase 一致）。
struct CalendarArgs: Decodable {
  let op: String
  let fromMs: Double?
  let toMs: Double?
  let limit: UInt32?
  let title: String?
  let startMs: Double?
  let endMs: Double?
  let allDay: Bool?
  let notes: String?
  let location: String?
}

enum CalendarError: Error, CustomStringConvertible {
  case badArgs(String)
  case denied(String)
  case noWritableCalendar

  var description: String {
    switch self {
    case .badArgs(let m): return "bad args: \(m)"
    case .denied(let m): return m
    case .noWritableCalendar:
      return "no writable calendar found — create one in the system Calendar app first"
    }
  }
}

enum CalendarAccess {
  /// 权限未授时的统一指引（文案与其它能力保持一致，指向设置页的可操作位置）。
  private static let hint =
    "calendar permission not granted — ask the user to tap Allow for Calendar in Settings → Agent"

  /// 确保拿到完整读写权限。
  ///
  /// **关键：这里绝不主动弹权限框。** 早期版本在 `.notDetermined` 时调
  /// `requestFullAccessToEvents` 并等用户作答 —— 真机实测后果是：agent 的
  /// 工具调用挂在那里等系统弹窗，直到 JS 侧 30s hostcall 超时才返回一个
  /// 毫无信息量的 "The operation timed out"，而且用户此刻可能正对着弹窗
  /// 犹豫。这与本仓库对 approval_request 的既有结论是同一条纪律
  /// （CONTRACTS §2.2：「禁止长挂起 fetch」）。
  ///
  /// 权限获取是 UI 的职责：用户在 设置 → Agent 的「设备能力」里点 Allow，
  /// 走 `native_request_permission` → `CalendarAccess.requestFullAccess`
  /// （那条路径本身就是 async 命令，弹窗期间不占宿主线程）。
  static func ensureFullAccess(_ invoke: Invoke, then: @escaping () -> Void) {
    if #available(iOS 17.0, *) {
      switch EKEventStore.authorizationStatus(for: .event) {
      case .fullAccess:
        then()
      case .writeOnly:
        // 只有写权限：读会失败。如实告知而不是返回空列表（空列表会让模型
        // 以为「用户这几天没安排」，那是错误结论）。
        invoke.reject("calendar access is write-only — grant full access to Calendar in Settings → Agent to read events")
      default:
        invoke.reject(hint)
      }
    } else {
      switch EKEventStore.authorizationStatus(for: .event) {
      case .authorized:
        then()
      default:
        invoke.reject(hint)
      }
    }
  }

  /// 当前权限状态（**同步、不弹窗**）—— 供设置页展示。
  ///
  /// 如实区分 `writeOnly`：只有写权限时读会失败，把它报成 granted 会让
  /// 用户以为能读（然后工具调用才失败，体验更差）。
  static func currentState() -> String {
    let status: EKAuthorizationStatus
    if #available(iOS 17.0, *) {
      status = EKEventStore.authorizationStatus(for: .event)
    } else {
      status = EKEventStore.authorizationStatus(for: .event)
    }
    switch status {
    case .fullAccess:
      return "granted"
    case .authorized:
      // iOS 17 之前只有 authorized（等价于完整访问）
      return "granted"
    case .notDetermined:
      return "prompt"
    case .denied, .restricted:
      return "denied"
    default:
      if #available(iOS 17.0, *), status == .writeOnly {
        return "writeOnly"
      }
      return "prompt"
    }
  }

  /// 主动请求日历完整权限（**仅供 `requestPermission` 命令调用**）。
  /// 用户未作答前不返回 —— 所以 Rust 侧命令必须是 async（不能占主线程）。
  static func requestFullAccess(_ invoke: Invoke) {
    let store = EKEventStore()
    if #available(iOS 17.0, *) {
      switch EKEventStore.authorizationStatus(for: .event) {
      case .fullAccess:
        invoke.resolve(["kind": "calendar", "granted": true])
      case .notDetermined:
        store.requestFullAccessToEvents { granted, _ in
          DispatchQueue.main.async { invoke.resolve(["kind": "calendar", "granted": granted]) }
        }
      default:
        invoke.resolve(["kind": "calendar", "granted": false])
      }
    } else {
      switch EKEventStore.authorizationStatus(for: .event) {
      case .authorized:
        invoke.resolve(["kind": "calendar", "granted": true])
      case .notDetermined:
        store.requestAccess(to: .event) { granted, _ in
          DispatchQueue.main.async { invoke.resolve(["kind": "calendar", "granted": granted]) }
        }
      default:
        invoke.resolve(["kind": "calendar", "granted": false])
      }
    }
  }
}

enum CalendarBridge {
  static func handle(_ args: CalendarArgs, _ invoke: Invoke) {
    // store 在这里才建：权限已经确认过了（ensureFullAccess 不弹窗）
    let store = EKEventStore()
    CalendarAccess.ensureFullAccess(invoke) {
      do {
        switch args.op {
        case "list": try list(args, store, invoke)
        case "create": try create(args, store, invoke)
        default: invoke.reject("calendar op must be list|create, got '\(args.op)'")
        }
      } catch {
        invoke.reject("\(error)")
      }
    }
  }

  // MARK: - list

  private static func list(_ args: CalendarArgs, _ store: EKEventStore, _ invoke: Invoke) throws {
    let nowMs = Date().timeIntervalSince1970 * 1000
    let from = Date(timeIntervalSince1970: (args.fromMs ?? nowMs) / 1000)
    let to = Date(timeIntervalSince1970: (args.toMs ?? (nowMs + 7 * 24 * 3600 * 1000)) / 1000)
    if to <= from {
      throw CalendarError.badArgs("toMs must be after fromMs")
    }
    // limit 默认 50：一次查询可能命中数百条，全塞进上下文既费 token 又淹掉
    // 真正相关的那几条。
    let limit = Int(args.limit ?? 50)

    let predicate = store.predicateForEvents(withStart: from, end: to, calendars: nil)
    let events = store.events(matching: predicate)
      .sorted { $0.startDate < $1.startDate }
      .prefix(limit)

    let out = events.map { event -> [String: Any] in
      var o: [String: Any] = [
        "id": event.eventIdentifier ?? "",
        "title": event.title ?? "(no title)",
        "startMs": event.startDate.timeIntervalSince1970 * 1000,
        "endMs": event.endDate.timeIntervalSince1970 * 1000,
        "allDay": event.isAllDay,
        "calendarName": event.calendar?.title ?? "",
      ]
      if let l = event.location, !l.isEmpty { o["location"] = l }
      if let n = event.notes, !n.isEmpty { o["notes"] = n }
      if let tz = event.timeZone?.identifier { o["timeZone"] = tz }
      return o
    }
    invoke.resolve([
      "events": out,
      "fromMs": from.timeIntervalSince1970 * 1000,
      "toMs": to.timeIntervalSince1970 * 1000,
    ])
  }

  // MARK: - create

  private static func create(_ args: CalendarArgs, _ store: EKEventStore, _ invoke: Invoke) throws {
    guard let title = args.title, !title.isEmpty else {
      throw CalendarError.badArgs("title is required for op=create")
    }
    guard let startMs = args.startMs else {
      throw CalendarError.badArgs("startMs is required for op=create")
    }
    // 未给结束时间时默认 1 小时：日历事件必须有 end，让模型每次都算一遍
    // 容易出错（也容易造出 0 长度事件）。
    let endMs = args.endMs ?? (startMs + 3600 * 1000)
    if endMs <= startMs {
      throw CalendarError.badArgs("endMs must be after startMs")
    }

    guard let calendar = store.defaultCalendarForNewEvents else {
      throw CalendarError.noWritableCalendar
    }

    let event = EKEvent(eventStore: store)
    event.calendar = calendar
    event.title = title
    event.startDate = Date(timeIntervalSince1970: startMs / 1000)
    event.endDate = Date(timeIntervalSince1970: endMs / 1000)
    event.isAllDay = args.allDay ?? false
    if let n = args.notes { event.notes = n }
    if let l = args.location { event.location = l }

    do {
      try store.save(event, span: .thisEvent, commit: true)
    } catch {
      // EventKit 的失败原因常常是权限/日历只读，原样带上更有用
      throw CalendarError.badArgs("save failed: \(error.localizedDescription)")
    }

    invoke.resolve([
      "id": event.eventIdentifier ?? "",
      "title": event.title ?? "",
      "startMs": event.startDate.timeIntervalSince1970 * 1000,
      "endMs": event.endDate.timeIntervalSince1970 * 1000,
      "allDay": event.isAllDay,
      "calendarName": event.calendar?.title ?? "",
    ])
  }
}
