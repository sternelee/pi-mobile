// Photos.kt —— 系统相册读取（MediaStore）。
//
// ## 只读，且不写入用户相册
//
// `list` 取元数据、`save` 把原图字节写到调用方指定路径（路径由 Rust 侧做完
// workspace jail 校验后传入）。不提供「保存到用户相册」。
//
// ## 权限：Android 13 分水岭
//
//   API 33+ : READ_MEDIA_IMAGES（细粒度媒体权限）
//   API ≤32 : READ_EXTERNAL_STORAGE
// 两个都在清单里声明，旧的带 android:maxSdkVersion="32"（否则新系统上会
// 出现一个用户看不懂的多余权限请求）。
//
// 注意：**不能用 WRITE_EXTERNAL_STORAGE 写回相册** —— Android 10+ 起
// 分区存储下它已失效，写回要走 MediaStore insert + IS_PENDING；
// 我们不做写回，所以完全不涉及。
//
// ## DATE_ADDED 的单位坑
//
// MediaStore 的 DATE_ADDED / DATE_TAKEN 是 **秒**，而对外协议一律
// **epoch 毫秒**（与日历/通讯录统一）。转换集中在本文件，避免各处漏乘 1000。

package com.sternelee.pinative

import android.Manifest
import android.app.Activity
import android.content.ContentUris
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.provider.MediaStore
import androidx.core.content.ContextCompat
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import java.io.File
import java.io.FileOutputStream

object PhotosBridge {

    private const val DEFAULT_LIMIT = 20
    private const val MAX_SAVE_BYTES = 20L * 1024 * 1024

    /** 当前平台需要的读媒体权限名。 */
    private fun readPermission(): String =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            Manifest.permission.READ_MEDIA_IMAGES
        } else {
            Manifest.permission.READ_EXTERNAL_STORAGE
        }

    fun hasReadPermission(c: Context) =
        ContextCompat.checkSelfPermission(c, readPermission()) == PackageManager.PERMISSION_GRANTED

    fun handle(activity: Activity, args: JSObject, invoke: Invoke) {
        if (!hasReadPermission(activity)) {
            invoke.reject("photos permission not granted — ask the user to tap Allow for 照片 in Settings → Agent")
            return
        }
        when (val op = args.optString("op", "list")) {
            "list" -> list(activity, args, invoke)
            "save" -> save(activity, args, invoke)
            else -> invoke.reject("photos op must be list|save, got '$op'")
        }
    }

    private fun list(activity: Activity, args: JSObject, invoke: Invoke) {
        val limit = if (args.has("limit")) args.optInt("limit", DEFAULT_LIMIT) else DEFAULT_LIMIT
        val fromMs = if (args.has("fromMs")) args.optLong("fromMs", 0L) else null
        val toMs = if (args.has("toMs")) args.optLong("toMs", 0L) else null

        val projection = arrayOf(
            MediaStore.Images.Media._ID,
            MediaStore.Images.Media.DISPLAY_NAME,
            MediaStore.Images.Media.DATE_ADDED,
            MediaStore.Images.Media.SIZE,
            MediaStore.Images.Media.WIDTH,
            MediaStore.Images.Media.HEIGHT,
            MediaStore.Images.Media.MIME_TYPE,
        )

        // 时间窗用秒（MediaStore 单位）；参数仍按毫秒绑定，避免忘了换算
        val selParts = ArrayList<String>()
        val selArgs = ArrayList<String>()
        fromMs?.let {
            selParts.add("${MediaStore.Images.Media.DATE_ADDED} >= ?")
            selArgs.add((it / 1000).toString())
        }
        toMs?.let {
            selParts.add("${MediaStore.Images.Media.DATE_ADDED} < ?")
            selArgs.add((it / 1000).toString())
        }
        val selection = if (selParts.isEmpty()) null else selParts.joinToString(" AND ")

        // 用 getContentUri(VOLUME_EXTERNAL) 而不是已废弃的 EXTERNAL_CONTENT_URI：
        // 后者在 Android 10+ 是被保留的兼容别名，某些 ROM 上可能解析到不含
        // 全部卷的旧 URI，表现为「明明有照片却查到 0 行」。
        val uri = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            MediaStore.Images.Media.getContentUri(MediaStore.VOLUME_EXTERNAL)
        } else {
            MediaStore.Images.Media.EXTERNAL_CONTENT_URI
        }

        val items = JSArray()
        var totalSeen = 0
        val cursor = try {
            activity.contentResolver.query(
                uri, projection, selection, selArgs.toTypedArray(),
                "${MediaStore.Images.Media.DATE_ADDED} DESC",
            )
        } catch (e: Exception) {
            invoke.reject("photos query failed: ${e.message}")
            return
        }
        // query() 返回 null 必须显式报错，不能静默给空数组 —— 那正是本项目
        // 被坑过两次的「ok 但形状是错的」：模型会把「查询失败」当成
        // 「相册里没有照片」，进而给出错误结论。
        if (cursor == null) {
            invoke.reject("photos query returned null cursor for ${uri} — provider unavailable?")
            return
        }
        cursor.use { c ->
            while (c.moveToNext()) {
                totalSeen++
                if (totalSeen > limit) continue
                val o = JSObject()
                o.put("id", c.getLong(0).toString())
                c.getString(1)?.let { o.put("filename", it) }
                // 秒 → 毫秒
                o.put("createdMs", c.getLong(2) * 1000)
                o.put("bytes", c.getLong(3))
                o.put("width", c.getInt(4))
                o.put("height", c.getInt(5))
                c.getString(6)?.let { o.put("mimeType", it) }
                items.put(o)
            }
        }

        val ret = JSObject()
        ret.put("photos", items)
        // totalSeen 是诊断也是产品信息：能区分「相册为空」与「被 limit 截断」，
        // 也便于判断「查询真的看到了行但没取出来」这类问题。
        ret.put("totalSeen", totalSeen)
        ret.put("source", uri.toString())
        invoke.resolve(ret)
    }

    private fun save(activity: Activity, args: JSObject, invoke: Invoke) {
        val id = args.optString("id", "")
        if (id.isEmpty()) {
            invoke.reject("photos save requires id")
            return
        }
        val dest = args.optString("destPath", "")
        if (dest.isEmpty()) {
            invoke.reject("photos save requires destPath")
            return
        }

        // 先查大小：超限就不开始读，避免把几十 MB 拉进内存再拒绝
        var size = -1L
        var mime: String? = null
        try {
            activity.contentResolver.query(
                ContentUris.withAppendedId(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, id.toLong()),
                arrayOf(MediaStore.Images.Media.SIZE, MediaStore.Images.Media.MIME_TYPE),
                null, null, null,
            )?.use { c ->
                if (c.moveToFirst()) {
                    size = c.getLong(0)
                    mime = c.getString(1)
                }
            }
        } catch (e: Exception) {
            invoke.reject("photos lookup failed: ${e.message}")
            return
        }
        if (size < 0) {
            invoke.reject("photo not found for id '$id'")
            return
        }
        if (size > MAX_SAVE_BYTES) {
            invoke.reject(
                "photo is too large to save: $size bytes (limit $MAX_SAVE_BYTES). " +
                    "Ask the user to pick a smaller image."
            )
            return
        }

        try {
            val uri = ContentUris.withAppendedId(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, id.toLong())
            val written: Long
            activity.contentResolver.openInputStream(uri).use { input ->
                if (input == null) {
                    invoke.reject("photos save: could not open stream for id '$id'")
                    return
                }
                // 先写临时文件再 rename：中途出错不会在 workspace 里留半个坏图片
                val target = File(dest)
                val tmp = File("${dest}.part")
                FileOutputStream(tmp).use { out -> written = input.copyTo(out) }
                if (!tmp.renameTo(target)) {
                    tmp.delete()
                    invoke.reject("photos save: could not finalize '$dest'")
                    return
                }
            }
            val ret = JSObject()
            ret.put("path", dest)
            ret.put("bytes", written)
            mime?.let { ret.put("mimeType", it) }
            ret.put("photoId", id)
            invoke.resolve(ret)
        } catch (e: Exception) {
            File("$dest.part").delete()
            invoke.reject("photos save failed: ${e.message}")
        }
    }
}
