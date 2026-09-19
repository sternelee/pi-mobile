import java.io.FileInputStream
import java.util.Properties

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

val keystoreProperties = Properties().apply {
    val file = rootProject.file("keystore.properties")
    if (file.exists()) file.inputStream().use { load(it) }
}

android {
    namespace = "dev.sterne.quickjsagent"
    compileSdk = 36

    defaultConfig {
        applicationId = "dev.sterne.quickjsagent"
        minSdk = 28          // 与主 App 一致（OpenSSL getentropy 需 API 28）
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
    }

    signingConfigs {
        create("release") {
            keyAlias = keystoreProperties["keyAlias"] as String
            keyPassword = keystoreProperties["password"] as String
            storeFile = file(keystoreProperties["storeFile"] as String)
            storePassword = keystoreProperties["password"] as String
        }
    }

    buildTypes {
        getByName("release") {
            signingConfig = signingConfigs.getByName("release")
            isMinifyEnabled = false   // 壳很薄，混淆没有收益，反而会拖慢构建
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    kotlinOptions {
        jvmTarget = "17"
    }

    // ⚠️ 关键：spike 二进制是以 `lib*.so` 的身份塞进 jniLibs 的，要**被解包到
    // nativeLibraryDir** 才能执行。AGP 默认 useLegacyPackaging=false（库直接从 APK
    // 内存映射、不落盘），那样就 exec 不到 —— 必须显式要求解包。
    packaging {
        jniLibs {
            useLegacyPackaging = true
        }
    }
}

dependencies {
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.core:core-ktx:1.13.1")
}
