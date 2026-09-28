package com.atelyx.desktop

import android.content.Context
import android.content.SharedPreferences
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/**
 * 系统级安全存储桥：Keystore 主密钥 + 加密 SharedPreferences。
 * Rust 凭据命令（API key / 应用秘密）经 webview JNI 线程调用本类静态方法；
 * 键名 = Rust 侧派生的条目名（provider-<sha256>-<id> / app-secret-<sha256>）。
 * 读取不存在返回 null（Rust 映射为空串）；删除幂等。
 */
object SecretStore {
  private const val PREFS_FILE = "atelyx-secure-prefs"
  private var cached: SharedPreferences? = null

  @Synchronized
  private fun prefs(context: Context): SharedPreferences {
    cached?.let { return it }
    val masterKey = MasterKey.Builder(context)
      .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
      .build()
    val prefs = EncryptedSharedPreferences.create(
      context,
      PREFS_FILE,
      masterKey,
      EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
      EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
    )
    cached = prefs
    return prefs
  }

  /** 保存条目（commit 同步落盘：调用方等待写完成）。 */
  @JvmStatic
  fun set(context: Context, key: String, value: String): Boolean =
    prefs(context).edit().putString(key, value).commit()

  /** 读取条目；不存在返回 null。 */
  @JvmStatic
  fun get(context: Context, key: String): String? = prefs(context).getString(key, null)

  /** 删除条目（幂等）。 */
  @JvmStatic
  fun delete(context: Context, key: String): Boolean =
    prefs(context).edit().remove(key).commit()
}
