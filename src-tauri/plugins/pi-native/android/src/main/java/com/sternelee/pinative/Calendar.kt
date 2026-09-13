// Calendar.kt —— 系统日历读写（CalendarContract）。
//
// 设计要点（与 iOS 侧 Calendar.swift 保持同一份协议）：
//   * 入参/出参一律 **epoch 毫秒** —— 两端都不解析 ISO8601，避免模型猜时区
//     或多处各自换算导致「差一天」的静默错误。
//   * `list` 默认 7 天窗口 + 50 条上限：一次查询可能命中数百条，全塞进
//     上下文既费 token 又淹掉真正相关的那几条。
//   * `create` 未给 endMs 时默认 +1 小时：日历事件必须有 end，让模型每次
//     都算一遍容易造出 0 长度事件。
//   * 写入前先确认存在**可写**日历（不是只读的节假日/订阅日历）——
//     否则会抛一个对模型毫无意义的底层错误。
//
// 时区：写入时带上系统默认时区 ID。CalendarContract 对 DTSTART/DTEND 期望
// UTC 毫秒，展示时用 EVENT_TIMEZONE 还原——不设置时区会让跨时区用户的
// 事件偏移。

package com.sternelee.pinative

import android.Manifest
import android.content.ContentUris
import android.content.ContentValues
import android.content.Context
import android.content.pm.PackageManager
import android.provider.CalendarContract
import androidx.core.content.ContextCompat
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject
import java.util.TimeZone

object CalendarBridge {

    private const val DEFAULT_WINDOW_MS = 7L * 24 * 3600 * 1000
    private const val DEFAULT_LIMIT = 50
    private const val DEFAULT_DURATION_MS = 3600L * 1000

    fun handle(activity: android.app.Activity, args: JSObject, invoke: Invoke) {
        val op = args.optString("op", "list")
        when (op) {
            "list" -> {
                if (!hasRead(activity)) {
                    invoke.reject("calendar read permission not granted — enable it in 设置")
                    return
                }
                list(activity, args, invoke)
            }
            "create" -> {
                if (!hasWrite(activity)) {
                    invoke.reject("calendar write permission not granted — enable it in 设置")
                    return
                }
                create(activity, args, invoke)
            }
            else -> invoke.reject("calendar op must be list|create, got '$op'")
        }
    }

    /** 读权限是否已授（供 permissionState 命令查询；同步、不弹窗）。 */
    fun hasReadPermission(c: Context) = hasRead(c)

    private fun hasRead(c: Context) =
        ContextCompat.checkSelfPermission(c, Manifest.permission.READ_CALENDAR) ==
            PackageManager.PERMISSION_GRANTED

    private fun hasWrite(c: Context) =
        ContextCompat.checkSelfPermission(c, Manifest.permission.WRITE_CALENDAR) ==
            PackageManager.PERMISSION_GRANTED

    private fun list(activity: android.app.Activity, args: JSObject, invoke: Invoke) {
        val now = System.currentTimeMillis()
        val from = if (args.has("fromMs")) args.optLong("fromMs", now) else now
        val to = if (args.has("toMs")) args.optLong("toMs", now + DEFAULT_WINDOW_MS) else now + DEFAULT_WINDOW_MS
        if (to <= from) {
            invoke.reject("bad args: toMs must be after fromMs")
            return
        }
        val limit = if (args.has("limit")) args.optInt("limit", DEFAULT_LIMIT) else DEFAULT_LIMIT

        // 日历名需要另查（CalendarContract 不在 Events 里给名字），
        // 先建 id→name 映射，避免每行一次查询。
        val calendarNames = HashMap<Long, String>()
        try {
            activity.contentResolver.query(
                CalendarContract.Calendars.CONTENT_URI,
                arrayOf(CalendarContract.Calendars._ID, CalendarContract.Calendars.CALENDAR_DISPLAY_NAME),
                null, null, null
            )?.use { c ->
                while (c.moveToNext()) calendarNames[c.getLong(0)] = c.getString(1) ?: ""
            }
        } catch (_: Exception) {
            // 拿不到名字不影响主流程（下面回落到 calendarId）
        }

        // 用 INSTANCE 表按时间窗口过滤才是正确做法（能覆盖重复事件展开），
        // Events 表只有原始行的 DTSTART。这里用 INSTANCE 以保证重复事件的
        // 每一次发生都能被查到 —— 且必须带 BEGIN/END 的 UTC 毫秒列。
        val builder = CalendarContract.Instances.CONTENT_URI.buildUpon()
        ContentUris.appendId(builder, from)
        ContentUris.appendId(builder, to)

        // 不加 DELETED 过滤：CalendarContract.Instances 这个类**不暴露**
        // DELETED 列（实测编译期 Unresolved reference），而 Instances 视图
        // 本身只返回有效发生实例，不需要额外排除。
        val events = JSArray()
        var count = 0
        try {
            activity.contentResolver.query(
                builder.build(),
                arrayOf(
                    CalendarContract.Instances.EVENT_ID,
                    CalendarContract.Instances.TITLE,
                    CalendarContract.Instances.BEGIN,
                    CalendarContract.Instances.END,
                    CalendarContract.Instances.ALL_DAY,
                    CalendarContract.Instances.EVENT_LOCATION,
                    CalendarContract.Instances.DESCRIPTION,
                    CalendarContract.Instances.CALENDAR_ID,
                    CalendarContract.Instances.EVENT_TIMEZONE,
                ),
                null, null,
                "${CalendarContract.Instances.BEGIN} ASC"
            )?.use { c ->
                while (c.moveToNext() && count < limit) {
                    val o = JSObject()
                    o.put("id", c.getLong(0).toString())
                    o.put("title", c.getString(1) ?: "(no title)")
                    o.put("startMs", c.getLong(2))
                    // END 可能为 null（无结束时间的原始事件）
                    if (!c.isNull(3)) o.put("endMs", c.getLong(3)) else o.put("endMs", c.getLong(2))
                    o.put("allDay", c.getInt(4) == 1)
                    c.getString(5)?.let { if (it.isNotEmpty()) o.put("location", it) }
                    c.getString(6)?.let { if (it.isNotEmpty()) o.put("notes", it) }
                    val calId = c.getLong(7)
                    o.put("calendarId", calId)
                    o.put("calendarName", calendarNames[calId] ?: calId.toString())
                    c.getString(8)?.let { o.put("timeZone", it) }
                    events.put(o)
                    count++
                }
            }
        } catch (e: Exception) {
            invoke.reject("calendar query failed: ${e.message}")
            return
        }

        val ret = JSObject()
        ret.put("events", events)
        ret.put("fromMs", from)
        ret.put("toMs", to)
        invoke.resolve(ret)
    }

    private fun create(activity: android.app.Activity, args: JSObject, invoke: Invoke) {
        val title = args.optString("title", "")
        if (title.isEmpty()) {
            invoke.reject("bad args: title is required for op=create")
            return
        }
        if (!args.has("startMs")) {
            invoke.reject("bad args: startMs is required for op=create")
            return
        }
        val startMs = args.optLong("startMs", 0L)
        val endMs = if (args.has("endMs")) args.optLong("endMs", 0L) else startMs + DEFAULT_DURATION_MS
        if (endMs <= startMs) {
            invoke.reject("bad args: endMs must be after startMs")
            return
        }

        val calendarId = writableCalendarId(activity)
        if (calendarId == null) {
            invoke.reject("no writable calendar found — create one in the system Calendar app first")
            return
        }

        val tz = TimeZone.getDefault().id
        val values = ContentValues().apply {
            put(CalendarContract.Events.CALENDAR_ID, calendarId)
            put(CalendarContract.Events.TITLE, title)
            put(CalendarContract.Events.DTSTART, startMs)
            put(CalendarContract.Events.DTEND, endMs)
            put(CalendarContract.Events.ALL_DAY, if (args.optBoolean("allDay", false)) 1 else 0)
            put(CalendarContract.Events.EVENT_TIMEZONE, tz)
            args.optString("notes", "").takeIf { it.isNotEmpty() }?.let {
                put(CalendarContract.Events.DESCRIPTION, it)
            }
            args.optString("location", "").takeIf { it.isNotEmpty() }?.let {
                put(CalendarContract.Events.EVENT_LOCATION, it)
            }
        }

        try {
            val uri = activity.contentResolver.insert(CalendarContract.Events.CONTENT_URI, values)
                ?: run {
                    invoke.reject("calendar insert returned no uri")
                    return
                }
            val id = uri.lastPathSegment ?: ""
            val ret = JSObject()
            ret.put("id", id)
            ret.put("title", title)
            ret.put("startMs", startMs)
            ret.put("endMs", endMs)
            ret.put("allDay", args.optBoolean("allDay", false))
            ret.put("calendarId", calendarId)
            ret.put("timeZone", tz)
            invoke.resolve(ret)
        } catch (e: Exception) {
            invoke.reject("calendar insert failed: ${e.message}")
        }
    }

    /**
     * 找一个**可写**日历：优先系统默认（IS_DEFAULT），其次任意 CALENDAR_ACCESS_LEVEL
     * 允许写入的本地日历。只读的节假日/订阅日历会导致写入失败，所以必须过滤。
     */
    private fun writableCalendarId(activity: android.app.Activity): Long? {
        val uri = CalendarContract.Calendars.CONTENT_URI
        val cols = arrayOf(
            CalendarContract.Calendars._ID,
            CalendarContract.Calendars.IS_PRIMARY,
            CalendarContract.Calendars.CALENDAR_ACCESS_LEVEL,
        )
        val writable = "${CalendarContract.Calendars.CALENDAR_ACCESS_LEVEL} >= ?"
        val writableArgs = arrayOf(CalendarContract.Calendars.CAL_ACCESS_CONTRIBUTOR.toString())
        try {
            activity.contentResolver.query(
                uri, cols, writable, writableArgs,
                "${CalendarContract.Calendars.IS_PRIMARY} DESC"
            )?.use { c ->
                if (c.moveToFirst()) return c.getLong(0)
            }
        } catch (_: Exception) {
            return null
        }
        return null
    }
}
