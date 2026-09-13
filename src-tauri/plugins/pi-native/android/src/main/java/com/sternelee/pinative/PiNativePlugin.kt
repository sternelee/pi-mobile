// PiNativePlugin.kt —— pi-mobile 自建设备能力的 Android 侧。
//
// 为什么 Android 定位不走 tauri-plugin-geolocation：
//   那个插件的实现是
//     LocationServices.getFusedLocationProviderClient(context)
//       .getCurrentLocation(prio, null)
//   —— 传了 null CancellationToken、没有超时。国内 ROM（实测 Honor MEY-AN00）
//   用高德（AMap）代理网络定位、GPS provider 不可用，Google fused provider
//   拿不到 fix 时回调既不 success 也不 failure → 永久挂起，最后被 JS 侧
//   30s hostcall 超时打断，报出无信息量的 "The operation timed out"。
//
// 本实现改用平台 LocationManager（该 ROM 会把它桥到高德代理），并补上：
//   1. 真超时（Handler.postDelayed + 一次性取消），到点必返回；
//   2. 先试 last-known（各 provider 取最新的一个），新鲜就直接用；
//   3. 超时后仍允许回退到 last-known，但必须带 fromLastKnown/staleMs，
//      让上层能判断「这是缓存位置」而不是假装成实时定位。
//
// 线程纪律（承 keepalive.rs 两次真机事故）：所有回调都在主线程 post 回
// Tauri 的 Invoke —— 不在回调里做阻塞活儿，异常一律转成 invoke.reject。

package com.sternelee.pinative

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.pm.PackageManager
import android.location.Location
import android.location.LocationListener
import android.location.LocationManager
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import androidx.core.content.ContextCompat
import app.tauri.PermissionState
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.Permission
import app.tauri.annotation.PermissionCallback
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

@InvokeArg
class LocationArgs {
    var highAccuracy: Boolean = false
    var timeoutMs: Long = 15000
}

/**
 * 定位结果的可信度分级 —— 决定要不要告诉用户「这是缓存位置」。
 */
private const val FRESH_ENOUGH_MS = 120_000L // 2 分钟内的 last-known 视为可用

@TauriPlugin(
    permissions = [
        Permission(
            strings = [
                Manifest.permission.ACCESS_FINE_LOCATION,
                Manifest.permission.ACCESS_COARSE_LOCATION
            ],
            alias = "location"
        ),
        Permission(
            strings = [
                Manifest.permission.READ_CALENDAR,
                Manifest.permission.WRITE_CALENDAR
            ],
            alias = "calendar"
        )
    ]
)
class PiNativePlugin(private val activity: Activity) : Plugin(activity) {

    override fun load(webView: android.webkit.WebView) {
        super.load(webView)
    }

    /**
     * 主动请求系统权限（弹窗）。目前只有日历 —— 定位/通知/剪贴板的授权
     * 走各自的官方插件。
     *
     * 用 requestPermissionForAlias：Tauri 的权限流程会把结果回调进
     * permissionCallback（已由 @PermissionCallback 注册），届时按
     * 实际授权状态如实 resolve。这样用户拒绝也能拿到明确答复，
     * 而不是一个含糊的异常。
     */
    @Command
    fun requestPermission(invoke: Invoke) {
        val args = invoke.getArgs()
        when (args.optString("kind", "")) {
            "calendar" -> requestPermissionForAlias("calendar", invoke, "permissionCallback")
            else -> invoke.reject("unknown permission kind '${args.optString("kind", "")}'")
        }
    }

    /**
     * 查询权限当前状态（同步、不弹窗）—— 设置页展示用。
     *
     * Android 无法可靠区分「未询问」与「已拒绝且不再询问」（要区分得自己
     * 记录是否问过）。这里如实报 granted 或 prompt —— 报 prompt 时 UI 给
     * 「Allow」按钮，用户点了若系统不再弹窗（已被永久拒绝），会看到系统
     * 无反应；所以 requestPermission 的结果里会带回最终状态，UI 据此更新。
     */
    @Command
    fun permissionState(invoke: Invoke) {
        val args = invoke.getArgs()
        when (args.optString("kind", "")) {
            "calendar" -> {
                val ret = JSObject()
                ret.put("kind", "calendar")
                ret.put(
                    "state",
                    if (CalendarBridge.hasReadPermission(activity)) "granted" else "prompt"
                )
                invoke.resolve(ret)
            }
            else -> invoke.reject("unknown permission kind '${args.optString("kind", "")}'")
        }
    }

    @PermissionCallback
    private fun permissionCallback(invoke: Invoke) {
        val granted = getPermissionState("calendar") == PermissionState.GRANTED
        val ret = JSObject()
        ret.put("kind", "calendar")
        ret.put("granted", granted)
        invoke.resolve(ret)
    }

    /// 日历读/写（CalendarContract）。实现见 Calendar.kt。
    @Command
    fun calendar(invoke: Invoke) {
        // 用 getArgs() 而不是 parseArgs(JSObject::class.java)：后者走 Jackson
        // 反序列化进 JSObject（org.json 风格，构造器吃 JSON 字符串），实测会
        // 得到一个**空对象** —— 于是 CalendarBridge 里 optString("op", "list")
        // 落到默认值，create 请求被当成 list 执行（真机表现为
        // calendar_create 返回了和 calendar_list 一样的列表，且因为“成功”
        // 而毫无报错）。getArgs() 直接用 argsJson 构造，才是原样的入参。
        val args = invoke.getArgs()
        CalendarBridge.handle(activity, args, invoke)
    }

    @Command
    fun location(invoke: Invoke) {
        val args = invoke.parseArgs(LocationArgs::class.java)
        try {
            if (!hasPermission()) {
                // 不用 requestPermission 流程：上层（native::location）已经先用
                // 官方插件的 check_permissions 判过权限并给出可读提示，
                // 走到这里说明权限状态与预期不一致 —— 直接如实报错更快定位。
                invoke.reject("location permission not granted")
                return
            }

            val lm = activity.getSystemService(Context.LOCATION_SERVICE) as LocationManager
            if (!lm.isLocationEnabled) {
                invoke.reject("location services are disabled on this device")
                return
            }

            // 1) 先看 last-known：各 provider 取最新的一个。
            bestLastKnown(lm)?.let { (loc, provider) ->
                val ageMs = System.currentTimeMillis() - loc.time
                if (ageMs <= FRESH_ENOUGH_MS) {
                    invoke.resolve(toJson(loc, provider, ageMs, fromLastKnown = true))
                    return
                }
            }

            // 2) 再等一次真实 fix，带真超时。
            requestFix(lm, args, invoke, fallback = bestLastKnown(lm))
        } catch (e: SecurityException) {
            invoke.reject("location permission denied at runtime: ${e.message}")
        } catch (e: Exception) {
            invoke.reject("location failed: ${e.message}")
        }
    }

    private fun hasPermission(): Boolean {
        val fine = ContextCompat.checkSelfPermission(activity, Manifest.permission.ACCESS_FINE_LOCATION)
        val coarse = ContextCompat.checkSelfPermission(activity, Manifest.permission.ACCESS_COARSE_LOCATION)
        return fine == PackageManager.PERMISSION_GRANTED || coarse == PackageManager.PERMISSION_GRANTED
    }

    /** 从所有 provider 的 last-known 里挑时间最新的一个。 */
    private fun bestLastKnown(lm: LocationManager): Pair<Location, String>? {
        var best: Location? = null
        var bestProvider: String? = null
        for (p in lm.allProviders) {
            val l = try {
                lm.getLastKnownLocation(p)
            } catch (_: SecurityException) {
                null
            } catch (_: Exception) {
                // 某些 ROM 上个别 provider 查询会抛 IllegalArgumentException
                null
            }
            if (l != null && (best == null || l.time > best!!.time)) {
                best = l
                bestProvider = p
            }
        }
        val b = best ?: return null
        return Pair(b, bestProvider ?: "unknown")
    }

    /**
     * 请求一次位置更新，并把「两条路必归一」写死：
     *   - onLocationChanged 先到 → 立即 resolve 并结算；
     *   - 超时先到 → 有 last-known 就回退（标注 fromLastKnown），否则明确报错。
     * `settled` 保证只结算一次（Late fix 会被丢弃，避免二次 resolve）。
     */
    private fun requestFix(
        lm: LocationManager,
        args: LocationArgs,
        invoke: Invoke,
        fallback: Pair<Location, String>?
    ) {
        val main = Handler(Looper.getMainLooper())
        var settled = false
        var listener: LocationListener? = null

        // 注意不能写 `listener?.let { ... }`：listener 是被闭包捕获的可变
        // 局部变量，Kotlin 拒绝 smart cast（编译期实测报
        // "Smart cast is impossible"）。先落到非空局部再操作即可。
        fun cleanup() {
            val l = listener ?: return
            try {
                lm.removeUpdates(l)
            } catch (_: Exception) {
                // provider 可能已被系统移除；取消失败不该影响结算
            }
            listener = null
        }

        val timeout = Runnable {
            if (settled) return@Runnable
            settled = true
            cleanup()
            if (fallback != null) {
                val (loc, provider) = fallback
                val ageMs = System.currentTimeMillis() - loc.time
                invoke.resolve(toJson(loc, provider, ageMs, fromLastKnown = true))
            } else {
                invoke.reject(
                    "no location fix within ${args.timeoutMs}ms and no cached location available " +
                        "— this ROM may lack a working GPS/network provider (try moving near a window " +
                        "or enabling a location source)"
                )
            }
        }

        listener = object : LocationListener {
            override fun onLocationChanged(location: Location) {
                if (settled) return
                settled = true
                main.removeCallbacks(timeout)
                cleanup()
                invoke.resolve(toJson(location, location.provider ?: "unknown", 0L, fromLastKnown = false))
            }

            // API 30+ 起这些回调可能被系统调用；旧签名在 compileSdk 36 下仍需覆写
            override fun onProviderEnabled(provider: String) {}
            override fun onProviderDisabled(provider: String) {}
            @Deprecated("Deprecated in Java")
            override fun onStatusChanged(provider: String?, status: Int, extras: Bundle?) {}
        }

        // provider 选择顺序：高精度优先 gps→fused→network；否则 network→fused。
        // 注意 filtered=false 拉全部，再用 requestLocationUpdates(provider,...)
        // 逐一尝试——某些 ROM 上 gps provider 存在但立刻报错，这时要能落到下一个。
        val candidates = if (args.highAccuracy) {
            listOf(LocationManager.GPS_PROVIDER, "fused", LocationManager.NETWORK_PROVIDER)
        } else {
            listOf(LocationManager.NETWORK_PROVIDER, "fused", LocationManager.GPS_PROVIDER)
        }

        // 同 cleanup 的理由：listener 被闭包捕获，不能直接 smart cast，
        // 先落非空局部再传进 requestLocationUpdates。
        val activeListener = listener ?: run {
            invoke.reject("internal: location listener not initialized")
            return
        }

        var registered = false
        for (p in candidates) {
            if (!lm.allProviders.contains(p)) continue
            try {
                lm.requestLocationUpdates(p, 0L, 0f, activeListener, Looper.getMainLooper())
                registered = true
                break
            } catch (_: SecurityException) {
                // 权限在注册瞬间被撤销（罕见）—— 继续试下一个 provider
            } catch (_: Exception) {
                // 该 provider 不可用，落下一个
            }
        }

        if (!registered) {
            settled = true
            cleanup()
            if (fallback != null) {
                val (loc, provider) = fallback
                invoke.resolve(
                    toJson(loc, provider, System.currentTimeMillis() - loc.time, fromLastKnown = true)
                )
            } else {
                invoke.reject("no usable location provider on this device")
            }
            return
        }

        main.postDelayed(timeout, args.timeoutMs)
    }

    /** Location → 协议 JSON（字段名与 Rust/JS 侧一一对应，camelCase）。 */
    private fun toJson(
        loc: Location,
        provider: String,
        staleMs: Long,
        fromLastKnown: Boolean
    ): JSObject {
        val o = JSObject()
        o.put("latitude", loc.latitude)
        o.put("longitude", loc.longitude)
        o.put("accuracyMeters", loc.accuracy.toDouble())
        if (loc.hasAltitude()) o.put("altitude", loc.altitude) else o.put("altitude", null)
        if (loc.hasBearing()) o.put("heading", loc.bearing.toDouble()) else o.put("heading", null)
        if (loc.hasSpeed()) o.put("speed", loc.speed.toDouble()) else o.put("speed", null)
        o.put("timestampMs", loc.time)
        o.put("provider", provider)
        o.put("staleMs", staleMs)
        o.put("fromLastKnown", fromLastKnown)
        return o
    }
}
