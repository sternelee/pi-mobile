package com.sternelee.pi_mobile

import android.Manifest
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.enableEdgeToEdge
import androidx.core.app.ActivityCompat

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    // M4 保活：Android 13+ 通知权限运行时请求（前台服务通知可见性依赖它）
    if (Build.VERSION.SDK_INT >= 33 &&
      ActivityCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) !=
      PackageManager.PERMISSION_GRANTED
    ) {
      ActivityCompat.requestPermissions(
        this,
        arrayOf(Manifest.permission.POST_NOTIFICATIONS),
        REQUEST_POST_NOTIFICATIONS,
      )
    }
  }

  companion object {
    private const val REQUEST_POST_NOTIFICATIONS = 0x7069
  }
}
