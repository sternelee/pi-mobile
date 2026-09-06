package com.sternelee.pi_mobile

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.IBinder

/**
 * 前台服务保活（M4）：agent 运行期由 Rust 经 JNI 驱动 start/update/stop，
 * 锁屏/切后台不再冻结多步任务。审批待决时切高优先级通道通知。
 *
 * 全部入口都是静态方法（Rust JNIEnv::call_static_method 直接可达，
 * 不需要 service 实例或 binder）。
 */
class ForegroundService : Service() {

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val channel = intent?.getStringExtra(EXTRA_CHANNEL) ?: CHANNEL_WORK
        val title = intent?.getStringExtra(EXTRA_TITLE) ?: "pi mobile"
        val text = intent?.getStringExtra(EXTRA_TEXT) ?: "agent working"
        startForeground(NOTIFY_ID, buildNotification(channel, title, text))
        return START_STICKY
    }

    private fun buildNotification(channel: String, title: String, text: String): Notification {
        val tap = PendingIntent.getActivity(
            this, 0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val builder = Notification.Builder(this, channel)
            .setContentTitle(title)
            .setContentText(text)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentIntent(tap)
            .setOngoing(true)
        return builder.build()
    }

    companion object {
        const val CHANNEL_WORK = "pi_agent_work"
        const val CHANNEL_APPROVAL = "pi_agent_approval"
        const val NOTIFY_ID = 0x7069
        const val EXTRA_CHANNEL = "channel"
        const val EXTRA_TITLE = "title"
        const val EXTRA_TEXT = "text"

        fun createChannels(context: Context) {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
            val mgr = context.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            mgr.createNotificationChannel(
                NotificationChannel(CHANNEL_WORK, "pi agent", NotificationManager.IMPORTANCE_LOW),
            )
            mgr.createNotificationChannel(
                NotificationChannel(CHANNEL_APPROVAL, "pi approvals", NotificationManager.IMPORTANCE_HIGH),
            )
        }

        /** agent_start：升前台（重复调用即刷新内容）。 */
        @JvmStatic
        fun start(context: Context, label: String) {
            notify(context, CHANNEL_WORK, "pi mobile", label)
        }

        /** 通知内容热切换（审批待决 → 高优先级；决策后回落工作态）。 */
        @JvmStatic
        fun notify(context: Context, channel: String, title: String, text: String) {
            createChannels(context)
            val intent = Intent(context, ForegroundService::class.java)
                .putExtra(EXTRA_CHANNEL, channel)
                .putExtra(EXTRA_TITLE, title)
                .putExtra(EXTRA_TEXT, text)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                context.startForegroundService(intent)
            } else {
                context.startService(intent)
            }
        }

        /** agent_end / agent_error：撤前台通知。 */
        @JvmStatic
        fun stop(context: Context) {
            context.stopService(Intent(context, ForegroundService::class.java))
        }
    }
}
