plugins {
    id("com.android.application")
}

android {
    namespace = "com.paintengine.android.app"
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
    // 引擎壳：PaintEngineView + libpaint_android.so（AAR 亦可经 mavenLocal 消费）
    implementation(project(":library"))
}
