package com.atelyx.desktop

import android.webkit.WebView
import android.os.Bundle
import androidx.activity.OnBackPressedCallback
import androidx.activity.enableEdgeToEdge

class MainActivity : TauriActivity() {
  private var webView: WebView? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)

    // 返回键交给前端逐层处理（浮层 → 视图内上级 → 回主页 → 二次确认退出）；
    // 前端未消费（含 WebView 尚未创建）时结束应用。
    onBackPressedDispatcher.addCallback(
      this,
      object : OnBackPressedCallback(true) {
        override fun handleOnBackPressed() {
          val view = webView
          if (view == null) {
            finish()
            return
          }
          view.evaluateJavascript("window.__atelyxAndroidBack?window.__atelyxAndroidBack():false") { handled ->
            if (handled != "true") finish()
          }
        }
      },
    )
  }

  override fun onWebViewCreate(webView: WebView) {
    this.webView = webView
  }
}
