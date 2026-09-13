plugins {
    id("com.android.library")
    id("org.jetbrains.kotlin.android")
}

android {
    namespace = "com.sternelee.pinative"
    compileSdk = 36

    defaultConfig {
        minSdk = 24
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }
    kotlinOptions {
        jvmTarget = "1.8"
    }
}

dependencies {
    // 只用 androidx.core 的 ContextCompat（权限查询）与 LocationManager.isLocationEnabled。
    // 刻意**不**依赖 play-services-location —— 本插件存在的理由就是绕开
    // Google fused provider（国内 ROM 上不可用），引入它反而会让人误以为
    // 可以用。
    implementation("androidx.core:core-ktx:1.9.0")
    implementation(project(":tauri-android"))
}
