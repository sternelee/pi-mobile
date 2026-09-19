buildscript {
    repositories {
        google()
        mavenCentral()
    }
    dependencies {
        // 与 src-tauri/gen/android 同一组版本（AGP 8.11.0 / Kotlin 1.9.25 / Gradle 8.14.3）
        classpath("com.android.tools.build:gradle:8.11.0")
        classpath("org.jetbrains.kotlin:kotlin-gradle-plugin:1.9.25")
    }
}
allprojects {
    repositories {
        google()
        mavenCentral()
    }
}
