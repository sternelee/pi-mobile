// Contacts.kt —— 系统通讯录读取（ContactsContract）。
//
// ## 只读
//
// 只实现 search / get，不做写入：agent 修改用户通讯录的风险与收益严重不对称
// （误改/误删联系人是不可逆的社交损失），且没有产品需求需要它。
//
// ## 为什么走 Data 表两次查询而不是一次 join
//
// ContactsContract 的经典模型：`Contacts` 表一行一个联系人（含聚合后的
// DISPLAY_NAME），具体字段（电话/邮箱/地址）在 `Data` 表里一字段一行。
// 所以是「先按名字/关键词查出一批 contact id，再按 id 批量取 Data」——
// 两次查询，但对每个联系人只多一次往返，且能一次拿全字段。
//
// ## 上下文体积
//
// 通讯录是最容易撑爆上下文的数据源：一个号码可能关联十几条字段。默认 limit
// 刻意压到 25（比日历的 50 低），并且只保留有值的字段（空字符串不输出）。

package com.sternelee.pinative

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.pm.PackageManager
import android.provider.ContactsContract
import androidx.core.content.ContextCompat
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSArray
import app.tauri.plugin.JSObject

object ContactsBridge {

    private const val DEFAULT_LIMIT = 25

    fun hasReadPermission(c: Context) =
        ContextCompat.checkSelfPermission(c, Manifest.permission.READ_CONTACTS) ==
            PackageManager.PERMISSION_GRANTED

    fun handle(activity: Activity, args: JSObject, invoke: Invoke) {
        if (!hasReadPermission(activity)) {
            invoke.reject("contacts permission not granted — ask the user to tap Allow for Contacts in Settings → Agent")
            return
        }
        when (val op = args.optString("op", "search")) {
            "search" -> search(activity, args, invoke)
            "get" -> get(activity, args, invoke)
            else -> invoke.reject("contacts op must be search|get, got '$op'")
        }
    }

    private fun search(activity: Activity, args: JSObject, invoke: Invoke) {
        val limit = if (args.has("limit")) args.optInt("limit", DEFAULT_LIMIT) else DEFAULT_LIMIT
        val query = args.optString("query", "").trim()

        // query 必填 —— 与 iOS 侧同一份契约。iOS 那边无关键词要走
        // `enumerateContacts`（枚举全部联系人）会卡死，故废掉无界分支；
        // Android 的无关键词排序虽然便宜，但两端行为不一致会让模型困惑。
        if (query.isEmpty()) {
            invoke.reject("contacts search requires a non-empty query (name substring)")
            return
        }

        val ids = ArrayList<String>()
        val names = HashMap<String, String>()

        // 关键词匹配 DISPLAY_NAME（大小写不敏感）。空关键词时按最近修改取前 N。
        val uri = ContactsContract.Contacts.CONTENT_URI
        val projection = arrayOf(
            ContactsContract.Contacts._ID,
            ContactsContract.Contacts.DISPLAY_NAME_PRIMARY,
        )
        // LIKE 的 % 必须作为参数绑定，不能拼进 selection（避免注入与转义问题）
        val selection = "${ContactsContract.Contacts.DISPLAY_NAME_PRIMARY} LIKE ?"
        val selArgs = arrayOf("%$query%")
        val order = "${ContactsContract.Contacts.DISPLAY_NAME_PRIMARY} ASC"

        try {
            activity.contentResolver.query(uri, projection, selection, selArgs, order)?.use { c ->
                while (c.moveToNext() && ids.size < limit) {
                    val id = c.getString(0) ?: continue
                    ids.add(id)
                    names[id] = c.getString(1) ?: ""
                }
            }
        } catch (e: Exception) {
            invoke.reject("contacts query failed: ${e.message}")
            return
        }

        val out = JSArray()
        for (id in ids) out.put(encode(activity, id, names[id] ?: ""))
        val ret = JSObject()
        ret.put("contacts", out)
        ret.put("query", query)
        invoke.resolve(ret)
    }

    private fun get(activity: Activity, args: JSObject, invoke: Invoke) {
        val id = args.optString("id", "")
        if (id.isEmpty()) {
            invoke.reject("contacts get requires id")
            return
        }
        // 先确认存在（不存在的 id 应报错而不是返回空对象 —— 空对象会让模型
        // 以为「这个联系人没有字段」）
        val exists = try {
            activity.contentResolver.query(
                ContactsContract.Contacts.CONTENT_URI,
                arrayOf(ContactsContract.Contacts._ID),
                "${ContactsContract.Contacts._ID} = ?", arrayOf(id), null
            )?.use { it.moveToFirst() } ?: false
        } catch (e: Exception) {
            invoke.reject("contacts lookup failed: ${e.message}")
            return
        }
        if (!exists) {
            invoke.reject("contact not found for id '$id'")
            return
        }
        val ret = JSObject()
        ret.put("contact", encode(activity, id, ""))
        invoke.resolve(ret)
    }

    /**
     * 取单个联系人的全部字段并编码。
     *
     * 只输出有值的字段：通讯录里大量字段是空的，全量输出会把上下文浪费在
     * `"givenName": ""` 这类噪声上。
     */
    private fun encode(activity: Activity, contactId: String, fallbackName: String): JSObject {
        val o = JSObject()
        o.put("id", contactId)

        val phones = JSArray()
        val emails = JSArray()
        val addresses = JSArray()
        var given: String? = null
        var family: String? = null
        var nickname: String? = null
        var org: String? = null
        var title: String? = null
        var display: String? = fallbackName.takeIf { it.isNotEmpty() }

        val dataUri = ContactsContract.Data.CONTENT_URI
        val projection = arrayOf(
            ContactsContract.Data.MIMETYPE,
            ContactsContract.Data.DATA1,
            ContactsContract.Data.DATA2, // type (label 的整数枚举)
            ContactsContract.Data.DATA3,
            ContactsContract.Data.DATA4,
            ContactsContract.Data.DATA5,
            ContactsContract.Data.DATA6,
            ContactsContract.Data.DATA7,
            ContactsContract.Data.DATA8,
            ContactsContract.Data.DATA9,
        )
        try {
            activity.contentResolver.query(
                dataUri, projection,
                "${ContactsContract.Data.CONTACT_ID} = ?", arrayOf(contactId), null
            )?.use { c ->
                while (c.moveToNext()) {
                    val mime = c.getString(0) ?: continue
                    when (mime) {
                        ContactsContract.CommonDataKinds.Phone.CONTENT_ITEM_TYPE -> {
                            val num = c.getString(1) ?: continue
                            val p = JSObject()
                            p.put("number", num)
                            typeLabel(c.getInt(2))?.let { p.put("label", it) }
                            phones.put(p)
                        }
                        ContactsContract.CommonDataKinds.Email.CONTENT_ITEM_TYPE -> {
                            val addr = c.getString(1) ?: continue
                            val e = JSObject()
                            e.put("address", addr)
                            typeLabel(c.getInt(2))?.let { e.put("label", it) }
                            emails.put(e)
                        }
                        ContactsContract.CommonDataKinds.StructuredPostal.CONTENT_ITEM_TYPE -> {
                            val parts = (1..9).mapNotNull { i ->
                                c.getString(i)?.takeIf { it.isNotBlank() }
                            }
                            if (parts.isNotEmpty()) {
                                val a = JSObject()
                                a.put("formatted", parts.joinToString(", "))
                                typeLabel(c.getInt(2))?.let { a.put("label", it) }
                                addresses.put(a)
                            }
                        }
                        ContactsContract.CommonDataKinds.StructuredName.CONTENT_ITEM_TYPE -> {
                            given = c.getString(2)?.takeIf { it.isNotBlank() }
                            family = c.getString(3)?.takeIf { it.isNotBlank() }
                            if (display.isNullOrEmpty()) {
                                display = c.getString(1)?.takeIf { it.isNotBlank() }
                            }
                        }
                        ContactsContract.CommonDataKinds.Nickname.CONTENT_ITEM_TYPE -> {
                            nickname = c.getString(1)?.takeIf { it.isNotBlank() }
                        }
                        ContactsContract.CommonDataKinds.Organization.CONTENT_ITEM_TYPE -> {
                            org = c.getString(1)?.takeIf { it.isNotBlank() }
                            title = c.getString(4)?.takeIf { it.isNotBlank() }
                        }
                    }
                }
            }
        } catch (e: Exception) {
            // 取详情失败不该让整个请求崩掉：返回已知的 id/name，让调用方能降级使用
            o.put("error", "partial: ${e.message}")
        }

        o.put("displayName", display ?: "")
        given?.let { o.put("givenName", it) }
        family?.let { o.put("familyName", it) }
        nickname?.let { o.put("nickname", it) }
        org?.let { o.put("organization", it) }
        title?.let { o.put("jobTitle", it) }
        if (phones.length() > 0) o.put("phones", phones)
        if (emails.length() > 0) o.put("emails", emails)
        if (addresses.length() > 0) o.put("addresses", addresses)
        return o
    }

    /**
     * DATA2 的类型整数 → 可读标签。
     *
     * 只翻常用项：完整映射需要 Type 与 Label 两列配合（自定义标签存在
     * Data.DATA3）。这里够用于「发给哪个号」的决策，未知类型返回 null
     * （宁可不给标签，也不要给一个错标签）。
     */
    private fun typeLabel(t: Int): String? = when (t) {
        ContactsContract.CommonDataKinds.Phone.TYPE_MOBILE -> "mobile"
        ContactsContract.CommonDataKinds.Phone.TYPE_HOME -> "home"
        ContactsContract.CommonDataKinds.Phone.TYPE_WORK -> "work"
        ContactsContract.CommonDataKinds.Phone.TYPE_MAIN -> "main"
        ContactsContract.CommonDataKinds.Phone.TYPE_FAX_WORK -> "fax work"
        ContactsContract.CommonDataKinds.Phone.TYPE_FAX_HOME -> "fax home"
        ContactsContract.CommonDataKinds.Phone.TYPE_PAGER -> "pager"
        ContactsContract.CommonDataKinds.Phone.TYPE_OTHER -> "other"
        ContactsContract.CommonDataKinds.Email.TYPE_HOME -> "home"
        ContactsContract.CommonDataKinds.Email.TYPE_WORK -> "work"
        ContactsContract.CommonDataKinds.Email.TYPE_MOBILE -> "mobile"
        ContactsContract.CommonDataKinds.Email.TYPE_OTHER -> "other"
        else -> null
    }
}
