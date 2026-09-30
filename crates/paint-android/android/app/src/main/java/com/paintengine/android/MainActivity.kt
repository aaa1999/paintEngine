package com.paintengine.android

import android.app.Activity
import android.content.Intent
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.pdf.PdfRenderer
import android.net.Uri
import android.os.Bundle
import android.os.ParcelFileDescriptor
import android.provider.MediaStore
import android.view.Gravity
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.HorizontalScrollView
import android.widget.LinearLayout
import android.widget.SeekBar
import android.widget.TextView
import android.widget.Toast
import java.io.OutputStream
import android.view.ViewTreeObserver
import kotlin.concurrent.thread
import java.nio.ByteBuffer

/**
 * paintEngine Android 演示 Activity。
 * 画布 + 两级工具条：主工具行（工具/操作）+ 上下文行（工具参数）。
 * 附件（图片/PDF）经系统选择器导入；PDF 由壳层渲染首页成位图走
 * 浮动放置（拖拽/旋转/缩放/提交），图片直接成层。
 */
class MainActivity : Activity() {

    private companion object {
        const val TAG = "paintEngine"

        /** 首帧后延时多久开始存档解码：让首帧先上屏 + idle 回报，不参与白屏。 */
        const val RESTORE_DELAY_MS = 100L
    }

    private lateinit var paintView: PaintEngineView

    // ── UI 部件（按需显隐）──
    private lateinit var contextRow: LinearLayout
    private lateinit var transformBar: LinearLayout
    private val toolButtons = mutableMapOf<Int, Button>()
    private var presetChipRow: LinearLayout? = null
    private var presetChips = mutableMapOf<String, TextView>()
    private var selectedPreset: String? = null

    // 形状工具当前偏好
    private var shapeKindCode = PaintEngineView.TOOL_RECT
    private var shapeFill = false

    // 帧率悬浮标签（设置项控制显隐）
    private lateinit var fpsLabel: Button

    // 定位小地图（设置项控制显隐）
    private lateinit var minimapView: MinimapView

    // 文字字号（对话框滑杆）
    private var textSize = 48f

    // 系统字体缓存（文字工具用）。后台预读线程与 UI 兜底路径都会触碰 → volatile
    @Volatile private var fontBytes: ByteArray? = null

    /** 字体已喂给引擎（setTextFont 每次都重新解析字体，须只喂一次）。 */
    private var fontOnEngine = false

    /** 后台预读的自动存档字节；首帧后就位。 */
    @Volatile private var pendingAutosave: ByteArray? = null

    /** 启动计时原点（Activity 实例化时刻，≈ 冷启动进程内最早可得）。 */
    private val bootT0 = android.os.SystemClock.elapsedRealtime()

    private val pickImage = 1001
    private val pickPdf = 1002

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        paintView = PaintEngineView(this)
        paintView.setBrushSize(18f)
        paintView.onTextAnchor = { cx, cy -> showTextInput(cx, cy) }

        val root = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        // 画布 + FPS 悬浮标签（点击开关，配置持久化）
        val stage = android.widget.FrameLayout(this)
        stage.addView(
            paintView,
            android.widget.FrameLayout.LayoutParams(
                android.widget.FrameLayout.LayoutParams.MATCH_PARENT,
                android.widget.FrameLayout.LayoutParams.MATCH_PARENT,
            ),
        )
        fpsLabel = Button(this).apply {
            text = "FPS --"
            setTextColor(android.graphics.Color.parseColor("#7FE388"))
            setBackgroundColor(0x99000000.toInt())
            setPadding(dp(8), dp(2), dp(8), dp(2))
            textSize = 11f
            typeface = android.graphics.Typeface.MONOSPACE
            minWidth = 0
            minHeight = 0
            stateListAnimator = null
            val lp = android.widget.FrameLayout.LayoutParams(
                android.widget.FrameLayout.LayoutParams.WRAP_CONTENT,
                android.widget.FrameLayout.LayoutParams.WRAP_CONTENT,
                android.view.Gravity.TOP or android.view.Gravity.START,
            )
            lp.leftMargin = dp(8)
            lp.topMargin = dp(36) // 避开状态栏（NoActionBar 下 stage 从屏幕顶起算）
            layoutParams = lp
        }
        minimapView = MinimapView(this, paintView)
        minimapView.visibility =
            if (getSharedPreferences("cfg", MODE_PRIVATE).getBoolean("minimap", true))
                View.VISIBLE else View.GONE
        applyFpsMonitor(getSharedPreferences("cfg", MODE_PRIVATE)
            .getBoolean("fps_monitor", true)) // 设置项默认开
        // 悬浮标签点击 = 快捷开关（正式入口在"设置"）
        fpsLabel.setOnClickListener { applyFpsMonitor(fpsLabel.visibility != View.VISIBLE) }
        stage.addView(fpsLabel)
        // 注意：不传 LayoutParams——WRAP_CONTENT 会让无内容测量的 View 铺满父容器
        // （ getDefaultSize AT_MOST 取 spec 尺寸），整个画布被半透明深底盖黑。
        // MinimapView init 里自设了 170dp 精确尺寸 + 右上角边距。
        stage.addView(minimapView)
        // 500ms 采样：帧率 = 呈现计数差值 / 采样间隔（引擎无平台时钟）
        var lastCount = 0L
        var lastTime = android.os.SystemClock.elapsedRealtime()
        var smoothed = 0f
        var fpsLogTick = 0
        val updater = object : Runnable {
            override fun run() {
                if (fpsLabel.visibility == View.VISIBLE) {
                    val now = android.os.SystemClock.elapsedRealtime()
                    val c = paintView.renderCount()
                    val dt = (now - lastTime) / 1000f
                    if (dt > 0.2f) {
                        val inst = (c - lastCount) / dt
                        smoothed = if (smoothed == 0f) inst else smoothed * 0.6f + inst * 0.4f
                        lastCount = c
                        lastTime = now
                    }
                    fpsLabel.text = "FPS ${"%.1f".format(smoothed)}"
                    // 帧率进日志：每 5s 一条
                    if (++fpsLogTick >= 10) {
                        fpsLogTick = 0
                        android.util.Log.i(TAG, "[fps] ${"%.1f".format(smoothed)}（呈现计数 ${paintView.renderCount()}）")
                    }
                }
                fpsLabel.postDelayed(this, 500)
            }
        }
        fpsLabel.post(updater)
        root.addView(
            stage,
            LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f,
            ),
        )

        // 附件放置操作条（浮动变换中显示）
        transformBar = buildTransformBar()
        root.addView(transformBar, rowParams())
        transformBar.visibility = View.GONE

        // 上下文行（随工具切换内容）
        contextRow = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(Color.parseColor("#EAEAEA"))
        }
        root.addView(contextRow, rowParams())

        // 主工具行
        root.addView(buildMainToolbar(), rowParams())
        setContentView(root)

        // 文字对象字体（懒加载系统字体，首次点击文字时设置）
        paintView.onTextAnchor = { cx, cy -> showTextInput(cx, cy) }
        paintView.onTextEdit = { t, sz, _ -> showTextInput(0.0, 0.0, existing = t, existingSize = sz) }

        // 启动关键路径优化（自 android-delivery 合并）：重 I/O（CJK 字体读取 /
        // 自动存档读取）移到后台线程预读，首帧就绪后才回 UI 线程落位。
        // 同步跑在 onCreate 里实测把首帧拖到 2.3s+（白屏主体）。
        warmStartupDataAsync()
        paintView.viewTreeObserver.addOnPreDrawListener(object : ViewTreeObserver.OnPreDrawListener {
            override fun onPreDraw(): Boolean {
                paintView.viewTreeObserver.removeOnPreDrawListener(this)
                android.util.Log.i(
                    TAG,
                    "[startup] 首帧就绪 ${android.os.SystemClock.elapsedRealtime() - bootT0}ms（自 Activity 构造）",
                )
                reportFullyDrawn()
                // 重落位（存档解码 ~2s）绝不能在 onPreDraw 里做——会把首帧堵住。
                // postDelayed：先让这一帧上屏、idle 信号回报，再开始解码。
                paintView.postDelayed({ applyStartupData() }, RESTORE_DELAY_MS)
                return true
            }
        })

        selectTool(PaintEngineView.TOOL_BRUSH)
        android.util.Log.i(
            TAG,
            "[startup] onCreate 完成 ${android.os.SystemClock.elapsedRealtime() - bootT0}ms",
        )
    }

    /** 后台预读启动期重文件：字体 + 自动存档（并行，不碰 UI/引擎）。 */
    private fun warmStartupDataAsync() {
        thread(name = "startup-font") {
            val t = android.os.SystemClock.elapsedRealtime()
            val b = loadFont()
            android.util.Log.i(
                TAG,
                "[startup] 字体预读 ${b?.size ?: 0} 字节（${android.os.SystemClock.elapsedRealtime() - t}ms，后台线程）",
            )
        }
        thread(name = "startup-autosave") {
            val f = autosaveFile()
            if (!f.exists()) {
                android.util.Log.i(TAG, "[restore] 无自动保存档")
                return@thread
            }
            val t = android.os.SystemClock.elapsedRealtime()
            pendingAutosave = runCatching { f.readBytes() }.getOrNull()
            android.util.Log.i(
                TAG,
                "[startup] 存档预读 ${pendingAutosave?.size ?: 0} 字节（${android.os.SystemClock.elapsedRealtime() - t}ms，后台线程）",
            )
        }
    }

    /** 首帧后的落位（UI 线程）：先恢复会话（内容优先），再喂字体。 */
    private fun applyStartupData() {
        val bytes = pendingAutosave
        if (bytes != null && paintView.editCount() == 0L) {
            val t = android.os.SystemClock.elapsedRealtime()
            val ok = runCatching { paintView.loadOra(bytes) }.getOrDefault(false)
            if (ok) {
                savedEditCount = paintView.editCount()
                android.util.Log.i(
                    TAG,
                    "[restore] 已恢复 ${bytes.size} 字节（解码 ${android.os.SystemClock.elapsedRealtime() - t}ms，首帧后）",
                )
                // 关键：loadOra 只置引擎脏区，必须 invalidate 触发 onDraw 重合成——
                // 否则画布停留在首帧前的空位图（透明黑）
                paintView.invalidate()
                Toast.makeText(this, "已恢复上次会话", Toast.LENGTH_SHORT).show()
            } else {
                android.util.Log.w(TAG, "[restore] 档案损坏，忽略")
            }
        } else if (bytes != null) {
            android.util.Log.i(TAG, "[restore] 首帧后用户已落笔，跳过恢复")
        }
        paintView.invalidate() // 无档也兜底一帧（尺寸就绪后的正式首绘）
        ensureFont()
    }

    private var savedEditCount = 0L

    /** 自动保存：onPause 落盘（Android 杀进程前必经）。 */
    override fun onPause() {
        super.onPause()
        android.util.Log.i(TAG, "[lifecycle] onPause → 自动保存检查")
        autosave()
    }

    private fun autosaveFile() = java.io.File(filesDir, "autosave.ora")

    private fun autosave() {
        if (paintView.editCount() == savedEditCount) {
            android.util.Log.d(TAG, "[autosave] 无修改，跳过")
            return
        }
        val t0 = android.os.SystemClock.elapsedRealtime()
        val bytes = paintView.saveOra() ?: run {
            android.util.Log.w(TAG, "[autosave] 存档失败（无内容？）")
            return
        }
        runCatching {
            autosaveFile().writeBytes(bytes)
            savedEditCount = paintView.editCount()
            android.util.Log.i(
                TAG,
                "[autosave] 已保存 ${bytes.size} 字节（${android.os.SystemClock.elapsedRealtime() - t0}ms）",
            )
        }.onFailure {
            android.util.Log.w(TAG, "[autosave] 写入失败: ${it.message}")
        }
    }


    /** 文字字体（对象光栅化）：读系统字体一次并设置（只喂引擎一次）。 */
    private fun ensureFont() {
        if (fontBytes == null) fontBytes = loadFont()
        val b = fontBytes
        if (!fontOnEngine && b != null) {
            val t = android.os.SystemClock.elapsedRealtime()
            paintView.setTextFont(b)
            fontOnEngine = true
            android.util.Log.i(
                TAG,
                "[startup] 字体已喂引擎（解析 ${android.os.SystemClock.elapsedRealtime() - t}ms）",
            )
        }
    }

    private fun rowParams() = LinearLayout.LayoutParams(
        LinearLayout.LayoutParams.MATCH_PARENT,
        LinearLayout.LayoutParams.WRAP_CONTENT,
    )

    // ── 主工具行 ──

    /** 形状主按钮的稳定键（工具码随描边/填充偏好变化，用负数固定槽位）。 */
    private val shapeSlot = -1

    private fun buildMainToolbar(): LinearLayout {
        val bar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setBackgroundColor(Color.parseColor("#F3F3F3"))
        }
        fun tool(label: String, slot: Int, codeProvider: () -> Int) = Button(this).apply {
            text = label
            setOnClickListener { selectTool(codeProvider()) }
            layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
            toolButtons[slot] = this
        }
        fun op(label: String, onClick: () -> Unit) = Button(this).apply {
            text = label
            setOnClickListener { onClick() }
            layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
        }
        bar.addView(tool("画笔", PaintEngineView.TOOL_BRUSH) { PaintEngineView.TOOL_BRUSH })
        bar.addView(tool("橡皮", PaintEngineView.TOOL_ERASER) { PaintEngineView.TOOL_ERASER })
        bar.addView(tool("形状", shapeSlot) { shapeKindWithFill() })
        bar.addView(tool("文字", PaintEngineView.TOOL_TEXT) { PaintEngineView.TOOL_TEXT })
        bar.addView(tool("填充", PaintEngineView.TOOL_FILL) { PaintEngineView.TOOL_FILL })
        bar.addView(op("附件") { showAttachChooser() })
        bar.addView(op("撤销") { paintView.undo(); paintView.invalidate() })
        bar.addView(op("重做") { paintView.redo(); paintView.invalidate() })
        bar.addView(op("图层") { showLayerDialog() })
        bar.addView(op("适配") { paintView.fitToContent(); paintView.invalidate() })
        bar.addView(op("保存") { savePng() })
        bar.addView(op("设置") { showSettingsDialog() })
        return bar
    }

    private fun shapeKindWithFill(): Int =
        shapeKindCode + if (shapeFill) 3 else 0

    private fun isShapeCode(code: Int) =
        code in PaintEngineView.TOOL_LINE..PaintEngineView.TOOL_ELLIPSE_FILL

    private fun selectTool(code: Int) {
        paintView.setToolCode(code)
        // 主按钮高亮：形状类都归到形状槽
        val activeSlot = if (isShapeCode(code)) shapeSlot else code
        for ((slot, b) in toolButtons) {
            b.setBackgroundColor(
                if (slot == activeSlot) Color.parseColor("#1677ff") else Color.TRANSPARENT,
            )
        }
        rebuildContextRow(code)
    }

    // ── 上下文行 ──

    private fun rebuildContextRow(code: Int) {
        contextRow.removeAllViews()
        presetChipRow = null
        when {
            code == PaintEngineView.TOOL_BRUSH || code == PaintEngineView.TOOL_ERASER -> {
                contextRow.addView(buildPresetStrip())
                contextRow.addView(buildSizeRow())
            }
            isShapeCode(code) -> contextRow.addView(buildShapeRow())
            code == PaintEngineView.TOOL_TEXT -> contextRow.addView(hint("点击文字可重新编辑；按住可拖动；空白处点击输入新文字"))
            code == PaintEngineView.TOOL_FILL -> contextRow.addView(hint("点击色块区域填充当前笔刷色"))
            else -> {} // 附件等无上下文
        }
    }

    private fun hint(text: String): TextView = TextView(this).apply {
        this.text = text
        gravity = Gravity.CENTER
        setPadding(0, dp(6), 0, dp(6))
        setTextColor(Color.parseColor("#555555"))
    }

    /** 笔刷预设横向条（内置 6 支 + 自定义）。 */
    private fun buildPresetStrip(): View {
        val scroll = HorizontalScrollView(this)
        presetChipRow = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        presetChips = mutableMapOf()
        val pad = dp(8)
        for (name in paintView.presetNames()) {
            val chip = TextView(this).apply {
                text = name
                setPadding(pad, pad / 2, pad, pad / 2)
                setTextColor(Color.BLACK)
                background = chipBg(selectedPreset == name)
                setOnClickListener {
                    selectedPreset = name
                    paintView.applyPreset(name)
                    for ((n, c) in presetChips) c.background = chipBg(n == name)
                    paintView.invalidate()
                }
            }
            presetChips[name] = chip
            presetChipRow?.addView(chip)
            val lp = LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.WRAP_CONTENT,
                LinearLayout.LayoutParams.WRAP_CONTENT,
            )
            lp.marginEnd = dp(4)
            chip.layoutParams = lp
        }
        scroll.addView(presetChipRow)
        scroll.setPadding(dp(4), dp(2), dp(4), dp(2))
        return scroll
    }

    private fun chipBg(selected: Boolean) = android.graphics.drawable.GradientDrawable().apply {
        cornerRadius = 16f
        setColor(if (selected) Color.parseColor("#1677ff") else Color.WHITE)
        setStroke(1, Color.parseColor("#CCCCCC"))
    }

    private fun buildSizeRow(): View {
        val row = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        row.addView(TextView(this).apply { text = "  笔号 " })
        val seek = SeekBar(this)
        seek.max = 199
        seek.progress = 17
        seek.setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
            override fun onProgressChanged(s: SeekBar?, v: Int, fromUser: Boolean) {
                paintView.setBrushSize((v + 1).toFloat())
                paintView.invalidate()
            }
            override fun onStartTrackingTouch(s: SeekBar?) {}
            override fun onStopTrackingTouch(s: SeekBar?) {}
        })
        row.addView(seek, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))
        return row
    }

    /** 形状上下文：线/矩/圆 + 描边/填充。 */
    private fun buildShapeRow(): View {
        val row = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        fun pick(label: String, code: Int) = Button(this).apply {
            text = label
            setOnClickListener {
                shapeKindCode = code
                selectTool(shapeKindWithFill())
            }
            layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
        }
        row.addView(pick("直线", PaintEngineView.TOOL_LINE))
        row.addView(pick("矩形", PaintEngineView.TOOL_RECT))
        row.addView(pick("椭圆", PaintEngineView.TOOL_ELLIPSE))
        row.addView(Button(this).apply {
            text = if (shapeFill) "填充" else "描边"
            setOnClickListener {
                shapeFill = !shapeFill
                selectTool(shapeKindWithFill())
            }
            layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
        })
        return row
    }

    // ── 文字输入 ──

    private fun showTextInput(canvasX: Double, canvasY: Double, existing: String? = null, existingSize: Float? = null) {
        val isEdit = existing != null
        if (existingSize != null) textSize = existingSize
        val input = EditText(this).apply {
            hint = "输入文字"
            if (isEdit) setText(existing)
        }
        val sizeRow = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        val sizeLabel = TextView(this)
        val seek = SeekBar(this).apply {
            max = 144 // 16..160
            progress = (textSize - 16).toInt()
        }
        seek.setOnSeekBarChangeListener(object : SeekBar.OnSeekBarChangeListener {
            override fun onProgressChanged(s: SeekBar?, v: Int, fromUser: Boolean) {
                textSize = (v + 16).toFloat()
                sizeLabel.text = "  字号 ${v + 16}  "
            }
            override fun onStartTrackingTouch(s: SeekBar?) {}
            override fun onStopTrackingTouch(s: SeekBar?) {}
        })
        sizeLabel.text = "  字号 ${textSize.toInt()}  "
        sizeRow.addView(sizeLabel)
        sizeRow.addView(seek, LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f))

        val panel = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(input, LinearLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
            addView(sizeRow)
        }
        val dialog = android.app.AlertDialog.Builder(this)
            .setTitle(if (isEdit) "编辑文字（拖动可移动）" else "文字")
            .setView(panel)
            .setPositiveButton(if (isEdit) "更新" else "插入") { _, _ ->
                val text = input.text.toString()
                if (text.isEmpty()) return@setPositiveButton
                ensureFont()
                val ok = if (isEdit) {
                    paintView.updateTextObject(text, textSize)
                } else {
                    // 锚点按"点击处=文字左上角"的直觉换算到基线原点
                    paintView.addTextObject(text, canvasX, canvasY + textSize * 0.8, textSize)
                }
                Toast.makeText(
                    this,
                    if (ok) (if (isEdit) "已更新" else "已插入") else "操作失败",
                    Toast.LENGTH_SHORT,
                ).show()
                paintView.invalidate()
            }
            .setNegativeButton("取消", null)
        if (isEdit) {
            dialog.setNeutralButton("删除") { _, _ ->
                paintView.deleteTextObject()
                Toast.makeText(this, "已删除", Toast.LENGTH_SHORT).show()
                paintView.invalidate()
            }
        }
        dialog.show()
    }

    /** 系统字体：CJK 优先，逐个回退，读一次缓存。 */
    private fun loadFont(): ByteArray? {
        fontBytes?.let { return it }
        val candidates = listOf(
            "/system/fonts/NotoSansCJK-Regular.ttc",
            "/system/fonts/DroidSansFallback.ttf",
            "/system/fonts/MiSans-Regular.ttf",
            "/system/fonts/NotoSansSC-Regular.otf",
            "/system/fonts/Roboto-Regular.ttf",
        )
        for (path in candidates) {
            runCatching {
                val f = java.io.File(path)
                if (f.exists() && f.length() > 50_000L) {
                    fontBytes = f.readBytes()
                    return fontBytes
                }
            }
        }
        return null
    }

    // ── 附件 ──

    private fun showAttachChooser() {
        android.app.AlertDialog.Builder(this)
            .setTitle("添加附件")
            .setItems(arrayOf("图片（PNG/JPG/WebP/SVG）", "PDF（首页）")) { _, which ->
                when (which) {
                    0 -> pickContent("image/*", pickImage)
                    1 -> pickContent("application/pdf", pickPdf)
                }
            }
            .show()
    }

    private fun pickContent(mime: String, code: Int) {
        val intent = Intent(Intent.ACTION_GET_CONTENT).apply {
            type = mime
            addCategory(Intent.CATEGORY_OPENABLE)
        }
        runCatching { startActivityForResult(Intent.createChooser(intent, "选择文件"), code) }
            .onFailure { Toast.makeText(this, "无法打开选择器", Toast.LENGTH_SHORT).show() }
    }

    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (resultCode != RESULT_OK) return
        val uri: Uri = data?.data ?: return
        when (requestCode) {
            pickImage -> importImageAttachment(uri)
            pickPdf -> importPdfAttachment(uri)
        }
    }

    private fun importImageAttachment(uri: Uri) {
        runCatching {
            val bytes = contentResolver.openInputStream(uri)?.use { it.readBytes() }
            if (bytes == null || !paintView.importImage(bytes)) {
                Toast.makeText(this, "导入失败（不支持的格式？）", Toast.LENGTH_SHORT).show()
            } else {
                Toast.makeText(this, "图片已添加为新图层", Toast.LENGTH_SHORT).show()
            }
            paintView.invalidate()
        }.onFailure {
            Toast.makeText(this, "读取失败：${it.message}", Toast.LENGTH_SHORT).show()
        }
    }

    /** PDF 首页 → 系统渲染位图 → 浮动层放置（拖拽/旋转/缩放/提交）。 */
    private fun importPdfAttachment(uri: Uri) {
        runCatching {
            val pfd: ParcelFileDescriptor =
                contentResolver.openFileDescriptor(uri, "r")
                    ?: throw IllegalStateException("无法打开 PDF")
            PdfRenderer(pfd).use { renderer ->
                if (renderer.pageCount == 0) throw IllegalStateException("PDF 无页面")
                renderer.openPage(0).use { page ->
                    val screenW = resources.displayMetrics.widthPixels
                    val scale = (screenW * 0.9 / page.width).coerceIn(1.0, 4.0)
                    var w = (page.width * scale).toInt()
                    var h = (page.height * scale).toInt()
                    // 渲染尺寸上限（位图内存保护）
                    val cap = 2200
                    if (w > cap || h > cap) {
                        val f = cap.toFloat() / maxOf(w, h)
                        w = (w * f).toInt(); h = (h * f).toInt()
                    }
                    val bmp = android.graphics.Bitmap.createBitmap(
                        w, h, android.graphics.Bitmap.Config.ARGB_8888,
                    )
                    // PDF 页可能透明，铺白底更符合"附件"观感
                    Canvas(bmp).drawColor(Color.WHITE)
                    page.render(bmp, null, null, PdfRenderer.Page.RENDER_MODE_FOR_DISPLAY)
                    val buf = ByteBuffer.allocate(w * h * 4)
                    bmp.copyPixelsToBuffer(buf)
                    bmp.recycle()
                    if (!paintView.pasteRgba(buf.array(), w, h)) {
                        throw IllegalStateException("放置失败")
                    }
                    transformBar.visibility = View.VISIBLE
                    Toast.makeText(this, "拖拽移动 · 按钮旋转/缩放 · ✓ 放置", Toast.LENGTH_LONG).show()
                }
            }
            pfd.close()
            paintView.invalidate()
        }.onFailure {
            transformBar.visibility = View.GONE
            Toast.makeText(this, "PDF 导入失败：${it.message}", Toast.LENGTH_SHORT).show()
        }
    }

    // ── 附件放置操作条 ──

    private fun buildTransformBar(): LinearLayout {
        val bar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setBackgroundColor(Color.parseColor("#FFF7E6"))
        }
        fun op(label: String, onClick: () -> Unit) = Button(this).apply {
            text = label
            setOnClickListener { onClick() }
            layoutParams = LinearLayout.LayoutParams(0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f)
        }
        bar.addView(op("↺") { paintView.transformRotate(-15.0); paintView.invalidate() })
        bar.addView(op("↻") { paintView.transformRotate(15.0); paintView.invalidate() })
        bar.addView(op("－") { paintView.transformScale(1 / 1.15); paintView.invalidate() })
        bar.addView(op("＋") { paintView.transformScale(1.15); paintView.invalidate() })
        bar.addView(op("✓ 放置") {
            if (paintView.commitTransform()) {
                transformBar.visibility = View.GONE
                Toast.makeText(this, "已放置", Toast.LENGTH_SHORT).show()
            }
            paintView.invalidate()
        })
        bar.addView(op("✕ 取消") {
            paintView.cancelTransform()
            transformBar.visibility = View.GONE
            paintView.invalidate()
        })
        return bar
    }

    // ── 图层（沿用） ──

    /**
     * 图层管理对话框：层列表（顶层在最上，绘画软件习惯）单选切换当前层；
     * 新建/合并带状态反馈。
     */
    private fun showLayerDialog() {
        val count = paintView.layerCount()
        if (count == 0) {
            Toast.makeText(this, "没有图层", Toast.LENGTH_SHORT).show()
            return
        }
        val indices = (0 until count).reversed().toList()
        val active = paintView.activeLayerIndex()
        val checkedPos = indices.indexOf(active).coerceAtLeast(0)
        // 单独显示状态（视图级，不持久；对话框会话内维护）
        var soloIdx = -1
        fun labels(): Array<String> = indices.mapIndexed { pos, idx ->
            val mark = if (idx == active) "● " else "○ "
            val solo = if (idx == soloIdx) "▶单独 " else ""
            val name = paintView.layerNameAt(idx) ?: ("图层 ${idx + 1}")
            mark + solo + name
        }.toTypedArray()

        val dialog = android.app.AlertDialog.Builder(this)
            .setTitle("图层（共 $count 层）· 点击单独显示，再点恢复叠加")
            .setSingleChoiceItems(labels(), checkedPos) { dlg, which ->
                val idx = indices[which]
                paintView.selectLayerIndex(idx)
                // 同层再点 = 取消单独；异层 = 单独该层
                soloIdx = if (soloIdx == idx) -1 else idx
                paintView.setSoloIndex(soloIdx)
                paintView.invalidate()
                Toast.makeText(
                    this,
                    if (soloIdx >= 0) "单独显示：${paintView.layerNameAt(idx) ?: (idx + 1)}" else "显示全部图层",
                    Toast.LENGTH_SHORT,
                ).show()
            }
            .setPositiveButton("新建") { _, _ ->
                paintView.addLayer()
                paintView.invalidate()
                Toast.makeText(this, "已新建图层（共 ${paintView.layerCount()} 层），之后画在新层上", Toast.LENGTH_SHORT).show()
            }
            .setNegativeButton("显示全部") { _, _ ->
                soloIdx = -1
                paintView.setSoloIndex(-1)
                paintView.invalidate()
            }
            .setNeutralButton("向下合并") { _, _ ->
                if (paintView.mergeDown()) {
                    paintView.invalidate()
                    Toast.makeText(this, "已合并（剩 ${paintView.layerCount()} 层）", Toast.LENGTH_SHORT).show()
                } else {
                    Toast.makeText(this, "底层无法再向下合并", Toast.LENGTH_SHORT).show()
                }
            }
            .show()
    }

    /** 帧率监控设置项应用：引擎开关 + 悬浮标签显隐 + 持久化。 */
    private fun applyFpsMonitor(on: Boolean) {
        getSharedPreferences("cfg", MODE_PRIVATE)
            .edit().putBoolean("fps_monitor", on).apply()
        paintView.setFpsMonitor(on)
        fpsLabel.visibility = if (on) View.VISIBLE else View.GONE
    }

    /** 设置项：帧率监控 / 网格点阵（SharedPreferences 持久化）。 */
    private fun showSettingsDialog() {
        val prefs = getSharedPreferences("cfg", MODE_PRIVATE)
        val items = arrayOf("帧率监控", "网格点阵", "定位小地图")
        val checked = booleanArrayOf(
            prefs.getBoolean("fps_monitor", true),
            prefs.getBoolean("show_grid", true),
            prefs.getBoolean("minimap", false),
        )
        android.app.AlertDialog.Builder(this)
            .setTitle("设置")
            .setMultiChoiceItems(items, checked) { _, which, isChecked ->
                when (which) {
                    0 -> applyFpsMonitor(isChecked)
                    1 -> {
                        prefs.edit().putBoolean("show_grid", isChecked).apply()
                        paintView.setShowGrid(isChecked)
                        paintView.invalidate()
                    }
                    2 -> {
                        prefs.edit().putBoolean("minimap", isChecked).apply()
                        minimapView.visibility = if (isChecked) View.VISIBLE else View.GONE
                        if (isChecked) minimapView.requestContent()
                    }
                }
            }
            .setPositiveButton("完成", null)
            .show()
    }

    /**
     * 定位小地图：右下角缩略视口。内容 = 文档包围盒缩略图（编辑后自动刷新）；
     * 红框 = 当前主视口范围；拖动/点按 = 把对应画布位置移到主视口中心。
     */
    private class MinimapView(
        ctx: android.content.Context,
        private val paintView: PaintEngineView,
    ) : android.view.View(ctx) {

        private var bmp: android.graphics.Bitmap? = null
        private var meta: FloatArray? = null  // ow, oh, bx, by, bw, bh
        private var lastEdit = -1L
        private var lastRefresh = 0L

        private val mmPaint = android.graphics.Paint(android.graphics.Paint.ANTI_ALIAS_FLAG)
        private val boxPaint = android.graphics.Paint().apply {
            color = android.graphics.Color.rgb(230, 60, 60)
            style = android.graphics.Paint.Style.STROKE
            strokeWidth = 3f
        }
        private val bgPaint = android.graphics.Paint().apply {
            color = 0xEE2A2A2E.toInt()
        }

        init {
            val dp = ctx.resources.displayMetrics.density
            val lp = android.widget.FrameLayout.LayoutParams(
                (170 * dp).toInt(), (170 * dp).toInt())
            lp.topMargin = (36 * dp).toInt() // 状态栏下方
            lp.rightMargin = (8 * dp).toInt()
            layoutParams = lp
            setPadding((6 * dp).toInt(), (6 * dp).toInt(), (6 * dp).toInt(), (6 * dp).toInt())
        }

        /** 内容刷新（编辑计数变化时自动节流 ≥1s；强制刷新入口）。 */
        fun requestContent() {
            refreshIfDue(force = true)
        }

        private fun refreshIfDue(force: Boolean) {
            val now = android.os.SystemClock.elapsedRealtime()
            val ec = paintView.editCount()
            if (!force && (ec == lastEdit || now - lastRefresh < 1000)) return
            val r = paintView.minimapPng(width.takeIf { it > 0 } ?: 400,
                height.takeIf { it > 0 } ?: 400) ?: return
            val png = r[0] as? ByteArray ?: return
            val m = r[1] as? FloatArray ?: return
            runCatching {
                bmp?.recycle()
                bmp = android.graphics.BitmapFactory.decodeByteArray(png, 0, png.size)
                meta = m
                lastEdit = ec
                lastRefresh = now
                invalidate()
            }
        }

        override fun onMeasure(widthSpec: Int, heightSpec: Int) {
            // 固定 170dp 正方形：无视 WRAP_CONTENT（默认 View 的 AT_MOST 会取满父尺寸）
            setMeasuredDimension(layoutParams.width, layoutParams.height)
        }

        override fun onDraw(canvas: android.graphics.Canvas) {
            val w = width.toFloat(); val h = height.toFloat()
            canvas.drawRoundRect(0f, 0f, w, h, 10f, 10f, bgPaint)
            val b = bmp ?: run { refreshIfDue(false); return }
            val m = meta ?: return
            // 缩略图按比例居中放入
            val scale = minOf((w - 12f) / m[0], (h - 12f) / m[1], 8f)
            val dw = m[0] * scale; val dh = m[1] * scale
            val dx = (w - dw) / 2f; val dy = (h - dh) / 2f
            mmPaint.alpha = 255
            canvas.drawBitmap(b, null, android.graphics.RectF(dx, dy, dx + dw, dy + dh), mmPaint)
            // 主视口指示框（画布 AABB → 缩略图坐标）
            val vr = paintView.viewportRect() ?: return
            val fx = { cx: Float -> dx + (cx - m[2]) / m[4] * dw }
            val fy = { cy: Float -> dy + (cy - m[3]) / m[5] * dh }
            canvas.drawRect(
                fx(vr[0]), fy(vr[1]),
                fx(vr[0] + vr[2]), fy(vr[1] + vr[3]),
                boxPaint,
            )
            // 节流刷新内容（编辑后）
            refreshIfDue(false)
        }

        override fun onTouchEvent(e: android.view.MotionEvent): Boolean {
            android.util.Log.i("paintEngine", "[minimap] touch ${bmp != null}")
            val b = bmp ?: return false
            val m = meta ?: return false
            val scale = minOf((width - 12f) / m[0], (height - 12f) / m[1], 8f)
            val dw = m[0] * scale; val dh = m[1] * scale
            val dx = (width - dw) / 2f; val dy = (height - dh) / 2f
            when (e.actionMasked) {
                android.view.MotionEvent.ACTION_DOWN,
                android.view.MotionEvent.ACTION_MOVE,
                -> {
                    // 缩略图坐标 → 画布坐标 → 主视口居中
                    val cx = (m[2] + (e.x - dx).coerceIn(0f, dw) / dw * m[4]).toDouble()
                    val cy = (m[3] + (e.y - dy).coerceIn(0f, dh) / dh * m[5]).toDouble()
                    paintView.viewportCenterOn(cx, cy)
                    paintView.invalidate()
                    invalidate()
                    return true
                }
            }
            return super.onTouchEvent(e)
        }
    }

    private fun savePng() {
        val png = paintView.exportPng() ?: run {
            Toast.makeText(this, "画布为空", Toast.LENGTH_SHORT).show()
            return
        }
        val values = android.content.ContentValues().apply {
            put(MediaStore.Images.Media.DISPLAY_NAME, "painting_${System.currentTimeMillis()}.png")
            put(MediaStore.Images.Media.MIME_TYPE, "image/png")
            put(MediaStore.Images.Media.RELATIVE_PATH, "Pictures/paintEngine")
        }
        val uri: Uri? = contentResolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values)
        if (uri == null) {
            Toast.makeText(this, "保存失败", Toast.LENGTH_SHORT).show()
            return
        }
        contentResolver.openOutputStream(uri)?.use { os: OutputStream -> os.write(png) }
        Toast.makeText(this, "已保存到 Pictures/paintEngine", Toast.LENGTH_SHORT).show()
    }

    private fun dp(v: Int): Int = (v * resources.displayMetrics.density).toInt()
}
