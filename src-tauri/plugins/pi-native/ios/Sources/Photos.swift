// Photos.swift —— 系统相册读取（Photos.framework）。
//
// ## 只读，且不写入用户相册
//
// 支持两件事：`list` 取元数据、`save` 把**原图字节写到调用方指定路径**
// （路径由 Rust 侧做完 workspace jail 校验后传入）。不提供「保存到用户相册」
// —— 那要额外权限（NSPhotoLibraryAddUsageDescription）且风险收益不对称。
//
// ## 权限：iOS 14 起有「受限访问」
//
//   .authorized        → granted
//   .limited           → limited（用户只选了部分照片）
//   .notDetermined     → prompt
//   .denied/.restricted→ denied
// `.limited` **不能报 granted**：那会让模型把「只看到部分照片」当成全部，
// 进而给出「你没有这张照片」这类错误结论。
//
// ## 与其它能力一致的纪律
//
// 工具调用**绝不主动弹权限框**（弹窗会把 agent 调用挂到 JS 侧超时；
// 日历/通讯录都踩过）。权限获取由 UI 的 requestPermission 负责。
//
// ## 崩溃教训（通讯录那次的延伸）
//
// 只用文档化 API、不手写「差不多够」的参数：`PHAsset` 属性读取与
// `PHImageManager.requestImageDataAndOrientation` 都是安全路径；相册里
// 没有像 `CNContactFormatter` 那种「需要你先声明一堆 key 否则抛 ObjC 异常」
// 的陷阱，但仍避免访问需要额外 entitlement 的属性。

import Foundation
import Photos
import Tauri
import UIKit

struct PhotosArgs: Decodable {
  let op: String
  let fromMs: Double?
  let toMs: Double?
  let limit: UInt32?
  let id: String?
  let destPath: String?
}

/// `@available(iOS 14.0, *)`：`PHPhotoLibrary.authorizationStatus(for:)` 与
/// `PHAuthorizationStatus.limited` 都是 iOS 14 起才有的 API，而 swift-rs 的
/// 实际编译目标低于 14（虽然 Package.swift 写了 `.iOS(.v14)`，但 swift-rs
/// 用自己的默认部署目标）—— 不加标注会直接编译失败。调用点相应做可用性守卫
/// （本 app 实际目标 iOS 16，所以 else 分支在真机上不会走到）。
@available(iOS 14.0, *)
enum PhotosBridge {
  /// save 的字节上限。相册原图动辄十几 MB，写进 workspace 会挤爆用户的
  /// 应用数据配额；超限直接拒绝并告知，而不是写一半留个坏文件。
  private static let maxSaveBytes = 20 * 1024 * 1024

  private static let hint =
    "photos permission not granted — ask the user to tap Allow for 照片 in Settings → Agent"

  static func currentState() -> String {
    switch PHPhotoLibrary.authorizationStatus(for: .readWrite) {
    case .authorized: return "granted"
    case .limited: return "limited"
    case .notDetermined: return "prompt"
    case .denied, .restricted: return "denied"
    @unknown default: return "prompt"
    }
  }

  /// 请求权限（**仅由 requestPermission 命令调用**，会弹窗）。
  static func requestAccess(_ invoke: Invoke) {
    let status = PHPhotoLibrary.authorizationStatus(for: .readWrite)
    switch status {
    case .authorized, .limited:
      invoke.resolve(["kind": "photos", "granted": true])
    case .notDetermined:
      PHPhotoLibrary.requestAuthorization(for: .readWrite) { newStatus in
        DispatchQueue.main.async {
          invoke.resolve([
            "kind": "photos",
            "granted": newStatus == .authorized || newStatus == .limited,
          ])
        }
      }
    default:
      invoke.resolve(["kind": "photos", "granted": false])
    }
  }

  /// 确保已授权（**不弹窗**）。
  static func ensureAccess(_ invoke: Invoke, then: @escaping (Bool) -> Void) {
    switch PHPhotoLibrary.authorizationStatus(for: .readWrite) {
    case .authorized:
      then(false)
    case .limited:
      // 受限可以读（只是不完整）——放行，但结果里带 limited 标记
      then(true)
    case .denied, .restricted:
      invoke.reject("photos access denied — enable it in Settings → Agent")
    default:
      invoke.reject(hint)
    }
  }

  static func handle(_ args: PhotosArgs, _ invoke: Invoke) {
    ensureAccess(invoke) { limited in
      switch args.op {
      case "list": list(args, invoke, limited: limited)
      case "save": save(args, invoke, limited: limited)
      default: invoke.reject("photos op must be list|save, got '\(args.op)'")
      }
    }
  }

  // MARK: - list

  private static func list(_ args: PhotosArgs, _ invoke: Invoke, limited: Bool) {
    let options = PHFetchOptions()
    // 只有图片：视频会让返回体与后续 save 的语义复杂化（时长、封面帧…），
    // 需要时再单独开。
    options.predicate = NSPredicate(format: "mediaType == %d", PHAssetMediaType.image.rawValue)

    var predicates: [NSPredicate] = []
    if let from = args.fromMs {
      predicates.append(
        NSPredicate(format: "creationDate >= %@", Date(timeIntervalSince1970: from / 1000) as NSDate))
    }
    if let to = args.toMs {
      predicates.append(
        NSPredicate(format: "creationDate < %@", Date(timeIntervalSince1970: to / 1000) as NSDate))
    }
    if !predicates.isEmpty {
      options.predicate = NSCompoundPredicate(andPredicateWithSubpredicates:
        [options.predicate!] + predicates)
    }
    options.sortDescriptors = [NSSortDescriptor(key: "creationDate", ascending: false)]
    // limit 默认 20：相册可能有几万张，且每条元数据都不小
    options.fetchLimit = Int(args.limit ?? 20)

    let result = PHAsset.fetchAssets(with: options)
    var items: [[String: Any]] = []
    items.reserveCapacity(result.count)
    result.enumerateObjects { asset, _, _ in
      var o: [String: Any] = [
        // localIdentifier 作为 id：save 时用它再取回同一个 asset
        "id": asset.localIdentifier,
        "width": asset.pixelWidth,
        "height": asset.pixelHeight,
        "favorite": asset.isFavorite,
      ]
      if let d = asset.creationDate { o["createdMs"] = d.timeIntervalSince1970 * 1000 }
      if let m = asset.modificationDate { o["modifiedMs"] = m.timeIntervalSince1970 * 1000 }
      // 仅当用户**授权了定位**且照片带 EXIF 位置时才有 —— 不额外请求权限
      if let loc = asset.location {
        o["latitude"] = loc.coordinate.latitude
        o["longitude"] = loc.coordinate.longitude
      }
      if let name = asset.value(forKey: "filename") as? String { o["filename"] = name }
      items.append(o)
    }

    var ret: [String: Any] = ["photos": items]
    if limited {
      ret["limited"] = true
      ret["note"] = "user granted access to only some photos; results may be incomplete"
    }
    invoke.resolve(ret)
  }

  // MARK: - save

  private static func save(_ args: PhotosArgs, _ invoke: Invoke, limited: Bool) {
    guard let id = args.id, !id.isEmpty else {
      invoke.reject("photos save requires id")
      return
    }
    guard let dest = args.destPath, !dest.isEmpty else {
      invoke.reject("photos save requires destPath")
      return
    }

    // 用 localIdentifier 反查同一个 asset。注意它可能已不存在（用户删了照片、
    // 或 iCloud 还没同步下来）——如实报错，而不是写一个空文件。
    let fetched = PHAsset.fetchAssets(withLocalIdentifiers: [id], options: nil)
    guard let asset = fetched.firstObject else {
      invoke.reject("photo not found for id '\(id)' (deleted, or not yet synced from iCloud)")
      return
    }

    let options = PHImageRequestOptions()
    options.version = .current
    options.deliveryMode = .highQualityFormat
    // iCloud 照片：允许走网络下载原图。不允许的话会静默返回降级缩略图 ——
    // 那样用户以为保存了原图，实际是压缩版，是最糟的失败方式。
    options.isNetworkAccessAllowed = true

    // 异步且**可能回调多次**（先给低清再给高清）。用 didDeliver 保证只结算一次。
    var settled = false
    PHImageManager.default().requestImageDataAndOrientation(for: asset, options: options) {
      data, uti, _, info in
      if settled { return }

      // 若这次回调是降级结果且高清还在路上，先别结算
      if let degraded = info?[PHImageResultIsDegradedKey] as? NSNumber, degraded.boolValue {
        return
      }
      settled = true

      if let err = info?[PHImageErrorKey] as? NSError {
        invoke.reject("reading photo failed: \(err.localizedDescription)")
        return
      }
      guard let data = data else {
        invoke.reject("reading photo returned no data")
        return
      }
      if data.count > maxSaveBytes {
        invoke.reject(
          "photo is too large to save: \(data.count) bytes (limit \(maxSaveBytes)). "
            + "Ask the user to pick a smaller image."
        )
        return
      }
      do {
        try data.write(to: URL(fileURLWithPath: dest), options: .atomic)
      } catch {
        invoke.reject("writing photo failed: \(error.localizedDescription)")
        return
      }
      invoke.resolve([
        "path": dest,
        "bytes": data.count,
        "mimeType": uti ?? "application/octet-stream",
        "photoId": id,
      ])
    }
  }
}
