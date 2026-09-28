# Rust 凭据命令经 JNI 按名解析桥类与方法，R8 不得改名/移除（Java/Kotlin 侧无引用，默认会被裁剪）
-keep class com.atelyx.desktop.SecretStore { *; }
# Rust 系统能力命令（存储授权 / 目录浏览 / 打开 URL）同样按名解析桥类
-keep class com.atelyx.desktop.PlatformOps { *; }
# 加密 SharedPreferences 底层的加密库字节码引用 JSR-305 注解，但发布件不带这些类；
# 注解只在编译期起作用、运行时不加载，因此按缺类告警处理，而非把注解库打进包
-dontwarn javax.annotation.Nullable
-dontwarn javax.annotation.concurrent.GuardedBy
