package com.atelyx.desktop

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.Settings
import androidx.core.content.FileProvider
import java.io.File

/**
 * 系统能力桥：设备名、存储授权状态/申请与外部 URL 打开。
 * Rust 侧（commands/mobile.rs）经 webview JNI 线程调用本类静态方法；
 * 类名与方法签名是跨语言契约，改名须同步 Rust 侧与 proguard 保留规则。
 */
object PlatformOps {
  /** 设备名（协作身份默认值）：安卓无用户可设主机名，用设备型号代替。 */
  @JvmStatic
  fun deviceName(context: Context): String = Build.MODEL.trim()

  /** 「所有文件访问权限」（MANAGE_EXTERNAL_STORAGE）是否已授予。 */
  @JvmStatic
  fun hasAllFilesAccess(context: Context): Boolean = Environment.isExternalStorageManager()

  /**
   * 外部存储根目录（/storage/emulated/0）：目录浏览的起点。
   * 文件系统根（/）对应用不可读，浏览必须从有权限的那一层开始。
   */
  @JvmStatic
  fun storageRoot(context: Context): String =
    Environment.getExternalStorageDirectory().absolutePath

  /**
   * 拉起「所有文件访问权限」的系统设置页：该权限不能弹窗申请，只能由用户手动开启。
   * 拉起即返回（不等待用户操作）；个别 ROM 没有按包名的设置页（ActivityNotFoundException），
   * 回落全局列表页；其余失败不吞，交由调用方按可读错误上报。
   */
  @JvmStatic
  fun requestAllFilesAccess(context: Context) {
    val perApp = Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION)
      .setData(Uri.fromParts("package", context.packageName, null))
    val fallback = Intent(Settings.ACTION_MANAGE_ALL_FILES_ACCESS_PERMISSION)
    try {
      context.startActivity(perApp)
    } catch (e: ActivityNotFoundException) {
      context.startActivity(fallback)
    }
  }

  /**
   * 用系统默认程序打开外部 URL。
   * 不做 resolveActivity 预判：包可见性下它会误报 null；打不开时由 startActivity 抛错上报。
   */
  @JvmStatic
  fun openUrl(context: Context, url: String) {
    val intent = Intent(Intent.ACTION_VIEW, Uri.parse(url))
      .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
    context.startActivity(intent)
  }

  /** 是否已允许安装未知来源应用（Android 8+ 装更新包的前置开关）。 */
  @JvmStatic
  fun canInstallPackages(context: Context): Boolean =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
      context.packageManager.canRequestPackageInstalls()
    } else {
      true
    }

  /**
   * 拉起「安装未知应用」的系统设置页：该开关不能弹窗申请，只能由用户手动开启。
   * 拉起即返回（不等待用户操作）；个别 ROM 没有按包名的页面，回落应用设置列表页。
   */
  @JvmStatic
  fun requestInstallPermission(context: Context) {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
      return
    }
    val perApp = Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES)
      .setData(Uri.fromParts("package", context.packageName, null))
    val fallback = Intent(Settings.ACTION_MANAGE_APPLICATIONS_SETTINGS)
    try {
      context.startActivity(perApp)
    } catch (e: ActivityNotFoundException) {
      context.startActivity(fallback)
    }
  }

  /**
   * 把已下载的更新包交给系统安装器。
   *
   * 先 rename 进 cacheDir（file_paths.xml 只暴露固定根目录，cache 在其中；同盘 rename 无拷贝）。
   * Android 7+ 必须经 FileProvider 传 content://，否则触发 FileUriExposedException。
   */
  @JvmStatic
  fun installApk(context: Context, path: String) {
    val source = File(path)
    if (!source.isFile) {
      throw IllegalArgumentException("安装包不存在：$path")
    }
    val updatesDir = File(context.cacheDir, "updates").apply { mkdirs() }
    val target = File(updatesDir, source.name)
    if (target.exists() && !target.delete()) {
      throw IllegalStateException("无法替换旧的安装包：${target.absolutePath}")
    }
    if (!source.renameTo(target)) {
      source.copyTo(target, overwrite = true)
      source.delete()
    }
    val uri = FileProvider.getUriForFile(context, "${context.packageName}.fileprovider", target)
    val intent = Intent(Intent.ACTION_VIEW)
      .setDataAndType(uri, "application/vnd.android.package-archive")
      .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_GRANT_READ_URI_PERMISSION)
    context.startActivity(intent)
  }
}
