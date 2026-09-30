import java.util.Properties

plugins {
    id("com.android.library")
    id("maven-publish")
}

android {
    namespace = "com.paintengine.android"
    compileSdk = 35

    defaultConfig {
        minSdk = 26
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

    // AAR 发布单一 release 变体（含源码 jar；.so 由 jniLibs 打入）
    publishing {
        singleVariant("release") {
            withSourcesJar()
        }
    }
}

dependencies {
    // 纯框架 UI，无 androidx 运行时依赖——宿主零传递依赖
}

// ── Maven 发布：./gradlew :library:publishToMavenLocal ──
// 宿主消费：repositories { mavenLocal() } +
//   implementation("com.paintengine.android:paint-engine:0.1.0")
afterEvaluate {
    publishing {
        publications {
            create<MavenPublication>("release") {
                from(components["release"])
                groupId = "com.paintengine.android"
                artifactId = "paint-engine"
                version = "0.1.0"
                pom {
                    name.set("paintEngine Android 壳")
                    description.set(
                        "Rust 绘图引擎（paint-core + paint-render）的 Android 库：" +
                            "PaintEngineView + libpaint_android.so。API 详见 crates/paint-android/ANDROID_API.md",
                    )
                    licenses {
                        license {
                            name.set("MIT")
                        }
                    }
                }
            }
        }
    }
}

// ── Rust .so 交叉编译（cargo-ndk）──
// 随 preBuild 自动联动，也可 ./gradlew :library:buildRust 单独执行。
// 模式（-PpaintEngine.rustBuild=...）：
//   auto（默认）工具链齐备才编，否则沿用 jniLibs 里既有的 .so
//   on         强制编译；缺工具链直接失败（CI 用）
//   off        完全跳过（纯 Kotlin 改动时提速）
val rustAbis = listOf("arm64-v8a", "x86_64")
val rustWorkspace = File(rootDir, "../../..") // android/ → paint-android → crates → 仓库根
val rustMode = providers.gradleProperty("paintEngine.rustBuild").orElse("auto")
val jniLibsDir = File(projectDir, "src/main/jniLibs")

// SDK 路径：local.properties 的 sdk.dir（AGP 同款来源）→ ANDROID_HOME 兜底
val sdkDir: String? = run {
    val props = Properties()
    val f = File(rootDir, "local.properties")
    if (f.exists()) f.inputStream().use { props.load(it) }
    props.getProperty("sdk.dir")
        ?: System.getenv("ANDROID_HOME")
        ?: System.getenv("ANDROID_SDK_ROOT")
}

/** 静默跑一条命令，返回退出码；命令不存在返回 -1。 */
fun probe(vararg cmd: String): Int = try {
    ProcessBuilder(*cmd)
        .redirectOutput(ProcessBuilder.Redirect.DISCARD)
        .redirectError(ProcessBuilder.Redirect.DISCARD)
        .start()
        .waitFor()
} catch (_: java.io.IOException) {
    -1
}

/** cargo-ndk 与两个 android target 是否齐备。 */
fun rustToolchainReady(): Boolean {
    if (probe("cargo", "--version") != 0) return false
    if (probe("cargo", "ndk", "--version") != 0) return false
    val installed = try {
        ProcessBuilder("rustup", "target", "list", "--installed")
            .start().inputStream.bufferedReader().readText()
    } catch (_: java.io.IOException) {
        return false
    }
    return installed.contains("aarch64-linux-android") &&
        installed.contains("x86_64-linux-android")
}

val buildRust = tasks.register("buildRust") {
    group = "rust"
    description = "cargo-ndk 交叉编译 libpaint_android.so 并拷入 library/src/main/jniLibs"
    // 不向 Gradle 声明 crates 源码输入：那会与 app 的原生元数据任务产生
    // 隐式依赖冲突。增量缓存交给 cargo 自身——无改动时本任务只是秒级 no-op。
    outputs.dir(jniLibsDir)
    outputs.upToDateWhen { false }

    doLast {
        val mode = rustMode.get()
        if (mode == "off") {
            logger.lifecycle("[buildRust] off——跳过，使用 jniLibs 既有 .so")
            return@doLast
        }
        if (!rustToolchainReady()) {
            val msg = "Rust 工具链不齐备（需要 cargo、cargo-ndk、" +
                "rustup target aarch64/x86_64-linux-android）"
            if (mode == "on") throw GradleException(msg)
            logger.lifecycle("[buildRust] $msg——auto 模式跳过，使用 jniLibs 既有 .so")
            return@doLast
        }
        val cmd = buildList {
            add("cargo")
            add("ndk")
            rustAbis.forEach { add("-t"); add(it) }
            add("-o"); add(jniLibsDir.absolutePath)
            add("build"); add("--release"); add("-p"); add("paint-android")
        }
        logger.lifecycle("[buildRust] ${cmd.joinToString(" ")}")
        val proc = ProcessBuilder(cmd)
            .directory(rustWorkspace)
            .apply {
                sdkDir?.let { environment()["ANDROID_HOME"] = it }
                redirectErrorStream(true)
            }
            .start()
        val output = proc.inputStream.bufferedReader().readText()
        logger.lifecycle(output.lines().takeLast(15).joinToString("\n"))
        if (proc.waitFor() != 0) {
            throw GradleException("cargo ndk 构建失败（最后输出见上；完整日志重跑 buildRust）")
        }
        rustAbis.forEach { abi ->
            val so = File(jniLibsDir, "$abi/libpaint_android.so")
            if (!so.exists()) throw GradleException("cargo ndk 未产出 $so")
            logger.lifecycle("[buildRust] $so（${so.length() / 1024} KiB）")
        }
    }
}

tasks.named("preBuild") { dependsOn(buildRust) }
