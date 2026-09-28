# Rust 凭据命令经 JNI 按名解析桥类与方法，R8 不得改名/移除（Java/Kotlin 侧无引用，默认会被裁剪）
-keep class com.atelyx.desktop.SecretStore { *; }
