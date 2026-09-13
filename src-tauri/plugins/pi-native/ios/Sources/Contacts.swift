// Contacts.swift —— 系统通讯录读取（Contacts.framework）。
//
// ## 只读
//
// 只实现 search / get，不做写入：agent 修改用户通讯录的风险与收益严重不对称
// （误改/误删联系人是不可逆的社交损失），且没有产品需求需要它。
//
// ## 权限语义（与日历不同，这里没有「只写」档）
//
//   .notDetermined → 未询问      → "prompt"
//   .authorized    → 已授权      → "granted"
//   .denied/.restricted → 拒绝   → "denied"
//   .limited（iOS 18+）→ 用户只授权了部分联系人。**不能报 granted**：
//     那会让模型以为查到的就是全部，从而对「找不到某人」给出错误结论。
//     这里如实报 "limited"，上层可提示用户「只授权了部分联系人」。
//
// ## 与日历一致的纪律
//
// 工具调用**绝不主动弹权限框**（ensureAccess 在 .notDetermined 时直接 reject
// 并指向设置页）。理由是实测过：弹窗会把 agent 调用挂到 JS 侧 30s 超时，
// 用户此刻可能正对着弹窗犹豫。

import Contacts
import Foundation
import Tauri

struct ContactsArgs: Decodable {
  let op: String
  let query: String?
  let id: String?
  let limit: UInt32?
}

enum ContactsBridge {
  private static let hint =
    "contacts permission not granted — ask the user to tap Allow for Contacts in Settings → Agent"

  private static let defaultLimit = 25

  /// 要取的字段。
  ///
  /// ⚠️ **`CNContactFormatter.descriptorForRequiredKeys(for:)` 是必需的**，
  /// 不能只靠手写 key 列表。真机崩溃栈（已存档）：
  ///   -[CNContactFormatter fullNameForContact:attributes:style:]
  ///     → -[CNContact contactType] → NSException → std::terminate → SIGTRAP
  /// 原因是 `CNContactFormatter.string(from:)` 会读 `contactType` 与姓名
  /// 前后缀/拼音等字段，而手写列表里没有 → Contacts 直接抛 ObjC 异常。
  /// **Swift 的 try/catch 抓不到 ObjC 异常**，于是它穿透到
  /// `std::terminate`，把整个 app 带崩（不是抛错、不是挂起 —— 进程直接死，
  /// 所以 JS 侧的超时也不会触发）。
  /// 用官方的 descriptor 是唯一稳妥做法：它由框架自己维护所需 key 集。
  ///
  /// 另外刻意**不含** `CNContactNoteKey`：备注需要
  /// `com.apple.developer.contacts.notes` entitlement（免费账号拿不到），
  /// 带上它会让整个 fetch 抛异常。
  private static var keys: [CNKeyDescriptor] {
    [
      CNContactFormatter.descriptorForRequiredKeys(for: .fullName),
      CNContactIdentifierKey as CNKeyDescriptor,
      CNContactTypeKey as CNKeyDescriptor,
      CNContactGivenNameKey as CNKeyDescriptor,
      CNContactFamilyNameKey as CNKeyDescriptor,
      CNContactMiddleNameKey as CNKeyDescriptor,
      CNContactNicknameKey as CNKeyDescriptor,
      CNContactOrganizationNameKey as CNKeyDescriptor,
      CNContactJobTitleKey as CNKeyDescriptor,
      CNContactPhoneNumbersKey as CNKeyDescriptor,
      CNContactEmailAddressesKey as CNKeyDescriptor,
      CNContactPostalAddressesKey as CNKeyDescriptor,
      CNContactBirthdayKey as CNKeyDescriptor,
    ]
  }

  static func currentState() -> String {
    switch CNContactStore.authorizationStatus(for: .contacts) {
    case .authorized: return "granted"
    case .notDetermined: return "prompt"
    case .denied, .restricted: return "denied"
    default:
      // .limited（iOS 18+）：部分授权。报 limited 而不是 granted ——
      // 报 granted 会让「查不到某人」被误读成「此人不在通讯录里」。
      if #available(iOS 18.0, *), CNContactStore.authorizationStatus(for: .contacts) == .limited {
        return "limited"
      }
      return "prompt"
    }
  }

  static func requestAccess(_ invoke: Invoke) {
    let store = CNContactStore()
    if #available(iOS 18.0, *), CNContactStore.authorizationStatus(for: .contacts) == .limited {
      // 已部分授权：再请求会弹「选择更多联系人」界面
      store.requestAccess(for: .contacts) { granted, _ in
        DispatchQueue.main.async { invoke.resolve(["kind": "contacts", "granted": granted]) }
      }
      return
    }
    switch CNContactStore.authorizationStatus(for: .contacts) {
    case .authorized, .limited:
      invoke.resolve(["kind": "contacts", "granted": true])
    case .notDetermined:
      store.requestAccess(for: .contacts) { granted, _ in
        DispatchQueue.main.async { invoke.resolve(["kind": "contacts", "granted": granted]) }
      }
    default:
      invoke.resolve(["kind": "contacts", "granted": false])
    }
  }

  /// 确保已授权（**不弹窗**）。
  static func ensureAccess(_ invoke: Invoke, then: @escaping () -> Void) {
    switch CNContactStore.authorizationStatus(for: .contacts) {
    case .authorized:
      then()
    case .denied, .restricted:
      invoke.reject("contacts access denied — enable it in Settings → Agent")
    default:
      if #available(iOS 18.0, *), CNContactStore.authorizationStatus(for: .contacts) == .limited {
        // 部分授权可以读（只是不完整）——放行，但结果里带上 note 说明。
        then()
      } else {
        invoke.reject(hint)
      }
    }
  }

  static func handle(_ args: ContactsArgs, _ invoke: Invoke) {
    ensureAccess(invoke) {
      do {
        switch args.op {
        case "search": try search(args, invoke)
        case "get": try get(args, invoke)
        default: invoke.reject("contacts op must be search|get, got '\(args.op)'")
        }
      } catch {
        invoke.reject("contacts \(args.op) failed: \(error.localizedDescription)")
      }
    }
  }

  // MARK: - search

  private static func search(_ args: ContactsArgs, _ invoke: Invoke) throws {
    let store = CNContactStore()
    let limit = Int(args.limit ?? UInt32(defaultLimit))
    let query = args.query?.trimmingCharacters(in: .whitespaces) ?? ""

    // **query 必填。** 早期版本在无关键词时走 `enumerateContacts` 取「最近
    // 若干条」，真机实测**卡死**：Apple 文档明确该 API 会枚举并排序**全部**
    // 联系人，iCloud 同步了几千条时开销极大，表现为 agent 调用挂到 JS 侧
    // 30s 超时。`predicateForContacts(matchingName:)` 才是系统索引化的路径，
    // 而且「按名字找一个人」本来就是通讯录工具的主要用途 —— 不做无界枚举。
    if query.isEmpty {
      invoke.reject("contacts search requires a non-empty query (name substring)")
      return
    }

    let pred = CNContact.predicateForContacts(matchingName: query)
    let contacts = try store.unifiedContacts(matching: pred, keysToFetch: keys)
      .prefix(limit)
      .map { $0 }

    var ret: [String: Any] = ["contacts": contacts.map(encode), "query": query]
    if #available(iOS 18.0, *), CNContactStore.authorizationStatus(for: .contacts) == .limited {
      // 如实告知结果可能不完整 —— 否则模型会断言「通讯录里没有这个人」
      ret["limited"] = true
      ret["note"] = "user granted access to only some contacts; results may be incomplete"
    }
    invoke.resolve(ret)
  }

  // MARK: - get

  private static func get(_ args: ContactsArgs, _ invoke: Invoke) throws {
    guard let id = args.id, !id.isEmpty else {
      invoke.reject("contacts get requires id")
      return
    }
    let store = CNContactStore()
    do {
      let c = try store.unifiedContact(withIdentifier: id, keysToFetch: keys)
      var ret: [String: Any] = ["contact": encode(c)]
      if #available(iOS 18.0, *), CNContactStore.authorizationStatus(for: .contacts) == .limited {
        ret["limited"] = true
      }
      invoke.resolve(ret)
    } catch {
      // CNError 的 notFound 是常见情况（用户删了/换了设备），如实报比空对象好
      invoke.reject("contact not found for id '\(id)': \(error.localizedDescription)")
    }
  }

  // MARK: - 编码

  private static func encode(_ c: CNContact) -> [String: Any] {
    var o: [String: Any] = [
      "id": c.identifier,
      "displayName": CNContactFormatter.string(from: c, style: .fullName) ?? "",
    ]
    if !c.givenName.isEmpty { o["givenName"] = c.givenName }
    if !c.familyName.isEmpty { o["familyName"] = c.familyName }
    if !c.middleName.isEmpty { o["middleName"] = c.middleName }
    if !c.nickname.isEmpty { o["nickname"] = c.nickname }
    if !c.organizationName.isEmpty { o["organization"] = c.organizationName }
    if !c.jobTitle.isEmpty { o["jobTitle"] = c.jobTitle }

    if !c.phoneNumbers.isEmpty {
      // 带 label（mobile/home/work）—— 模型要选「发到哪个号」时这是关键信息
      o["phones"] = c.phoneNumbers.map { v -> [String: Any] in
        var p: [String: Any] = ["number": v.value.stringValue]
        let l = CNLabeledValue<NSString>.localizedString(forLabel: v.label ?? "")
        if !l.isEmpty { p["label"] = l }
        return p
      }
    }
    if !c.emailAddresses.isEmpty {
      o["emails"] = c.emailAddresses.map { v -> [String: Any] in
        var e: [String: Any] = ["address": v.value as String]
        let l = CNLabeledValue<NSString>.localizedString(forLabel: v.label ?? "")
        if !l.isEmpty { e["label"] = l }
        return e
      }
    }
    if !c.postalAddresses.isEmpty {
      let fmt = CNPostalAddressFormatter()
      o["addresses"] = c.postalAddresses.map { v -> [String: Any] in
        var a: [String: Any] = [
          "formatted": fmt.string(from: v.value).replacingOccurrences(of: "\n", with: ", ")
        ]
        let l = CNLabeledValue<NSString>.localizedString(forLabel: v.label ?? "")
        if !l.isEmpty { a["label"] = l }
        return a
      }
    }
    if let b = c.birthday, let d = Calendar.current.date(from: b) {
      o["birthdayMs"] = d.timeIntervalSince1970 * 1000
    }
    return o
  }
}
