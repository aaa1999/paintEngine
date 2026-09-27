plugins {
    id("com.android.application")
}

android {
    namespace = "com.paintengine.android"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.paintengine.android"
        minSdk = 26
        targetSdk = 35
        versionCode = 1
        versionName = "0.1.0"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

}

dependencies {
    // 纯框架 UI，无 androidx 运行时依赖——缩小 APK 与构建依赖面
}
