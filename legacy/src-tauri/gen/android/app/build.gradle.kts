import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("rust")
}

val tauriProperties = Properties().apply {
    val propFile = file("tauri.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

val appVersionName = tauriProperties.getProperty("tauri.android.versionName", "1.0")

val keystoreProperties = Properties().apply {
    val propFile = rootProject.file("keystore.properties")
    if (propFile.exists()) {
        propFile.inputStream().use { load(it) }
    }
}

android {
    compileSdk = 36
    namespace = "com.atelyx.desktop"
    defaultConfig {
        // 明文放行：协作服务端多为局域网 http://<ip>:<port>，地址由用户自填无法预置白名单
        manifestPlaceholders["usesCleartextTraffic"] = "true"
        applicationId = "com.atelyx.desktop"
        minSdk = 35
        targetSdk = 36
        versionCode = tauriProperties.getProperty("tauri.android.versionCode", "1").toInt()
        versionName = appVersionName
    }
    // 发布形态 = 单文件覆盖全部 ABI（fat APK）。显式声明而非沿用 rust 插件的默认列表，
    // 避免 CLI 的 --target 参数改写 ABI 过滤后，产物内容与包名里的 universal 不符
    productFlavors {
        getByName("universal") {
            ndk {
                abiFilters.clear()
                abiFilters += listOf("arm64-v8a", "armeabi-v7a", "x86", "x86_64")
            }
        }
    }
    signingConfigs {
        if (keystoreProperties.isNotEmpty()) {
            // 显式核对四项：缺项时给可读错误，避免构建到一半才因空值失败
            listOf("storeFile", "storePassword", "keyAlias", "keyPassword").forEach { key ->
                check(keystoreProperties.getProperty(key) != null) { "keystore.properties 缺少 $key" }
            }
            create("release") {
                storeFile = file(keystoreProperties.getProperty("storeFile"))
                storePassword = keystoreProperties.getProperty("storePassword")
                keyAlias = keystoreProperties.getProperty("keyAlias")
                keyPassword = keystoreProperties.getProperty("keyPassword")
            }
        }
    }
    buildTypes {
        getByName("debug") {
            isDebuggable = true
            isJniDebuggable = true
            isMinifyEnabled = false
            packaging {                jniLibs.keepDebugSymbols.add("*/arm64-v8a/*.so")
                jniLibs.keepDebugSymbols.add("*/armeabi-v7a/*.so")
                jniLibs.keepDebugSymbols.add("*/x86/*.so")
                jniLibs.keepDebugSymbols.add("*/x86_64/*.so")
            }
        }
        getByName("release") {
            isMinifyEnabled = true
            // 显式列举而非递归拾取：tauri 生成物在源码树里，用 fileTree 还会顺带扫到 build 目录的产物
            proguardFiles(
                *listOf(
                    file("src/main/java/com/atelyx/desktop/generated/proguard-wry.pro"),
                    file("proguard-tauri.pro"),
                    file("proguard-rules.pro"),
                ).plus(getDefaultProguardFile("proguard-android-optimize.txt"))
                    .toTypedArray()
            )
            // 有签名配置才关联；缺失时保持未签名（开发构建不因缺密钥而失败）
            signingConfigs.findByName("release")?.let { signingConfig = it }
        }
    }
    // Java 与 Kotlin 目标版本需一致显式声明：AGP 8 起两者不再联动，只改一处会仍停留在 8
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }
    buildFeatures {
        buildConfig = true
    }
    // 图标资源单一来源 = src-tauri/icons/android（tauri icon 产物），不在工程内再放一份防漂移
    sourceSets {
        getByName("main") {
            res.setSrcDirs(listOf("src/main/res", "../../../icons/android"))
        }
    }
    // 发布名自带版本与架构，上传 Release 不必手工改名（debug 产物保持 AGP 默认名）
    applicationVariants.all {
        if (buildType.name == "release") {
            outputs.forEach { output ->
                (output as com.android.build.gradle.internal.api.BaseVariantOutputImpl)
                    .outputFileName = "Atelyx_${appVersionName}.apk"
            }
        }
    }
}

rust {
    rootDirRel = "../../../"
}

dependencies {
    implementation("androidx.webkit:webkit:1.14.0")
    implementation("androidx.appcompat:appcompat:1.7.1")
    implementation("androidx.activity:activity-ktx:1.10.1")
    implementation("com.google.android.material:material:1.12.0")
    implementation("androidx.lifecycle:lifecycle-process:2.10.0")
    // 凭据安全存储（SecretStore.kt）：Keystore 主密钥 + 加密 SharedPreferences
    implementation("androidx.security:security-crypto:1.1.0")
    testImplementation("junit:junit:4.13.2")
    androidTestImplementation("androidx.test.ext:junit:1.1.4")
    androidTestImplementation("androidx.test.espresso:espresso-core:3.5.0")
}

apply(from = "tauri.build.gradle.kts")