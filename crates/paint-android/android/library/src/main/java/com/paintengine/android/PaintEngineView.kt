package com.paintengine.android

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.util.AttributeSet
import android.view.MotionEvent
import android.view.View

/**
 * paintEngine 的 Android 视图。
 *
 * - 呈现：onDraw 时经 nativeRender 把引擎帧写入 ARGB_8888 Bitmap 再上屏
 * - 输入：onTouchEvent 把 MotionEvent（含 getHistorical* 批量历史点、
 *   压感、工具类型）翻译转发；数位笔悬停经 onGenericMotionEvent 驱动手掌拒绝
 * - 手势：双指平移缩放由引擎内置状态机处理，本层只转发原始事件
 * - 文字：引擎 Text 工具点击落锚点后，本层抬手时取锚点回调宿主弹输入
 * - 变换：附件放置（浮动层）期间单指拖拽驱动平移，宿主用按钮旋转/缩放/提交
 *
 * 坐标：MotionEvent 的 view 坐标即物理像素，与引擎视口坐标系一致。
 */
class PaintEngineView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
) : View(context, attrs) {

    companion object {
        init {
            System.loadLibrary("paint_android")
        }

        // 与 Rust 侧 lib.rs 约定
        private const val PHASE_DOWN = 0
        private const val PHASE_MOVE = 1
        private const val PHASE_UP = 2
        private const val PHASE_CANCEL = 3

        private const val KIND_PEN = 0
        private const val KIND_ERASER = 1
        private const val KIND_TOUCH = 2
        private const val KIND_MOUSE = 3

        // 工具码（与 Rust 侧 tool_from_code 一致）
        const val TOOL_BRUSH = 0
        const val TOOL_ERASER = 1
        const val TOOL_MASK = 2
        const val TOOL_LINE = 3
        const val TOOL_RECT = 4
        const val TOOL_ELLIPSE = 5
        const val TOOL_LINE_FILL = 6
        const val TOOL_RECT_FILL = 7
        const val TOOL_ELLIPSE_FILL = 8
        const val TOOL_TEXT = 9
        const val TOOL_FILL = 10
    }

    private external fun nativeCreate(): Long
    private external fun nativeDestroy(handle: Long)
    private external fun nativeResize(handle: Long, w: Int, h: Int, scale: Float)
    private external fun nativePointer(
        handle: Long, phase: Int, id: Int,
        x: Double, y: Double, pressure: Double,
        tiltX: Double, tiltY: Double, kind: Int, tUs: Long,
    )
    private external fun nativePenInRange(handle: Long, inRange: Boolean)
    private external fun nativeFocus(handle: Long, focused: Boolean)
    private external fun nativeRender(handle: Long, bitmap: Bitmap): Boolean
    private external fun nativeSetTool(handle: Long, code: Int)
    private external fun nativeTakeTextAnchor(handle: Long): FloatArray?
    private external fun nativeDrawText(
        handle: Long, font: ByteArray, text: String,
        x: Double, y: Double, size: Double,
    ): Boolean
    private external fun nativeSetTextFont(handle: Long, font: ByteArray)
    private external fun nativeAddTextObject(
        handle: Long, text: String, x: Double, y: Double, size: Double,
    ): Boolean
    private external fun nativeTakeTextEdit(handle: Long): Array<String>?
    private external fun nativeUpdateTextObject(handle: Long, text: String, size: Double): Boolean
    private external fun nativeDeleteTextObject(handle: Long): Boolean
    private external fun nativeEditCount(handle: Long): Long
    private external fun nativeRenderCount(handle: Long): Long
    private external fun nativeSetFpsMonitor(handle: Long, on: Boolean)
    private external fun nativeSaveOra(handle: Long): ByteArray?
    private external fun nativeLoadOra(handle: Long, data: ByteArray): Boolean
    private external fun nativeSetBrushSize(handle: Long, size: Double)
    private external fun nativeSetBrushColor(handle: Long, r: Int, g: Int, b: Int)
    private external fun nativePresetNames(handle: Long): Array<String>?
    private external fun nativeApplyPreset(handle: Long, name: String): Boolean
    private external fun nativeUndo(handle: Long): Boolean
    private external fun nativeRedo(handle: Long): Boolean
    private external fun nativeAddLayer(handle: Long): Boolean
    private external fun nativeMergeDown(handle: Long): Boolean
    private external fun nativeFlatten(handle: Long): Boolean
    private external fun nativeLayerCount(handle: Long): Int
    private external fun nativeActiveLayerIndex(handle: Long): Int
    private external fun nativeSelectLayerIndex(handle: Long, index: Int): Boolean
    private external fun nativeLayerNameAt(handle: Long, index: Int): String?
    private external fun nativeFitToContent(handle: Long)
    private external fun nativeZoom100(handle: Long)
    private external fun nativeSetShowGrid(handle: Long, show: Boolean)
    private external fun nativeExportPng(handle: Long): ByteArray?
    private external fun nativeImportPng(handle: Long, data: ByteArray): Boolean
    private external fun nativeImportImage(handle: Long, data: ByteArray): Boolean
    private external fun nativePasteRgba(handle: Long, rgba: ByteArray, w: Int, h: Int): Boolean
    private external fun nativeTransforming(handle: Long): Boolean
    private external fun nativeTransformTranslateScreen(handle: Long, dx: Double, dy: Double)
    private external fun nativeTransformRotate(handle: Long, deltaDeg: Double)
    private external fun nativeTransformScale(handle: Long, factor: Double)
    private external fun nativeCommitTransform(handle: Long): Boolean
    private external fun nativeCancelTransform(handle: Long): Boolean

    private var handle: Long = 0L
    private var bitmap: Bitmap? = null

    /** 文字锚点回调（新文字；宿主弹输入框后调 [addTextObject]）。 */
    var onTextAnchor: ((canvasX: Double, canvasY: Double) -> Unit)? = null

    /** 命中已有文字对象回调（编辑；宿主弹预填编辑框）。 */
    var onTextEdit: ((text: String, size: Float, colorHex: String) -> Unit)? = null

    /** 本层镜像的工具码（轮询文字锚点用）。 */
    var currentToolCode: Int = TOOL_BRUSH
        private set

    // 变换模式下的拖拽参考点（view 坐标）
    private var transformDragLast: Pair<Float, Float>? = null

    val engineHandle: Long get() = handle

    init {
        handle = nativeCreate()
    }

    override fun onAttachedToWindow() {
        super.onAttachedToWindow()
        if (handle == 0L) handle = nativeCreate()
    }

    override fun onDetachedFromWindow() {
        if (handle != 0L) {
            nativeDestroy(handle)
            handle = 0L
        }
        bitmap?.recycle()
        bitmap = null
        super.onDetachedFromWindow()
    }

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        super.onSizeChanged(w, h, oldw, oldh)
        if (w <= 0 || h <= 0 || handle == 0L) return
        bitmap?.recycle()
        bitmap = Bitmap.createBitmap(w, h, Bitmap.Config.ARGB_8888)
        nativeResize(handle, w, h, resources.displayMetrics.density.toFloat())
        invalidate()
    }

    override fun onDraw(canvas: Canvas) {
        val b = bitmap
        if (handle != 0L && b != null && nativeRender(handle, b)) {
            canvas.drawBitmap(b, 0f, 0f, null)
        }
    }

    override fun onWindowFocusChanged(hasWindowFocus: Boolean) {
        super.onWindowFocusChanged(hasWindowFocus)
        if (handle != 0L) nativeFocus(handle, hasWindowFocus)
    }

    // ── 输入 ──

    override fun onTouchEvent(event: MotionEvent): Boolean {
        if (handle == 0L) return false

        // 变换模式（附件放置）：单指拖拽 = 平移浮动内容
        if (nativeTransforming(handle)) {
            when (event.actionMasked) {
                MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN ->
                    transformDragLast = event.x to event.y
                MotionEvent.ACTION_MOVE -> {
                    val last = transformDragLast
                    if (last != null && event.pointerCount == 1) {
                        nativeTransformTranslateScreen(
                            handle,
                            (event.x - last.first).toDouble(),
                            (event.y - last.second).toDouble(),
                        )
                    }
                    transformDragLast = event.x to event.y
                }
                MotionEvent.ACTION_UP, MotionEvent.ACTION_CANCEL -> transformDragLast = null
            }
            invalidate()
            return true
        }
        transformDragLast = null

        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> {
                val i = event.actionIndex
                dispatchPointer(event, i, PHASE_DOWN, event.eventTime)
            }
            MotionEvent.ACTION_MOVE -> {
                // 关键：展开批量历史点，否则高频输入被系统合并后丢采样
                for (hist in 0 until event.historySize) {
                    for (i in 0 until event.pointerCount) {
                        dispatchPointer(event, i, PHASE_MOVE, event.getHistoricalEventTime(hist), hist)
                    }
                }
                for (i in 0 until event.pointerCount) {
                    dispatchPointer(event, i, PHASE_MOVE, event.eventTime)
                }
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> {
                val i = event.actionIndex
                dispatchPointer(event, i, PHASE_UP, event.eventTime)
            }
            MotionEvent.ACTION_CANCEL -> {
                for (i in 0 until event.pointerCount) {
                    dispatchPointer(event, i, PHASE_CANCEL, event.eventTime)
                }
            }
        }

        // 文字工具：抬手后先查"命中已有对象"（编辑），否则新锚点
        if (currentToolCode == TOOL_TEXT &&
            (event.actionMasked == MotionEvent.ACTION_UP || event.actionMasked == MotionEvent.ACTION_POINTER_UP)
        ) {
            val edit = nativeTakeTextEdit(handle)
            if (edit != null && edit.size >= 3) {
                onTextEdit?.invoke(edit[0], edit[1].toFloatOrNull() ?: 48f, edit[2])
            } else {
                nativeTakeTextAnchor(handle)?.let { xy ->
                    if (xy.size >= 2) onTextAnchor?.invoke(xy[0].toDouble(), xy[1].toDouble())
                }
            }
        }

        invalidate()
        return true
    }

    override fun onGenericMotionEvent(event: MotionEvent): Boolean {
        // 数位笔悬停 → PenInRange（手掌拒绝）
        if (handle != 0L && event.getToolType(0) == MotionEvent.TOOL_TYPE_STYLUS) {
            when (event.actionMasked) {
                MotionEvent.ACTION_HOVER_ENTER,
                MotionEvent.ACTION_HOVER_MOVE,
                -> {
                    nativePenInRange(handle, true)
                    return true
                }
                MotionEvent.ACTION_HOVER_EXIT -> {
                    nativePenInRange(handle, false)
                    return true
                }
            }
        }
        return super.onGenericMotionEvent(event)
    }

    private fun dispatchPointer(e: MotionEvent, i: Int, phase: Int, tMs: Long, hist: Int = -1) {
        val kind = when (e.getToolType(i)) {
            MotionEvent.TOOL_TYPE_STYLUS -> KIND_PEN
            MotionEvent.TOOL_TYPE_ERASER -> KIND_ERASER
            MotionEvent.TOOL_TYPE_FINGER -> KIND_TOUCH
            else -> KIND_MOUSE
        }
        // 手指/鼠标压感噪声大且无意义，传 -1 让引擎按满压处理
        val pressure = if (kind == KIND_PEN || kind == KIND_ERASER) {
            e.getPressure(i).toDouble()
        } else -1.0
        // 笔倾斜：AXIS_ORIENTATION（方位角）+ AXIS_TILT（离垂直倾角）
        // → W3C tiltX/tiltY 投影；直立笔（tilt≈0）传 NaN
        var tiltX = Double.NaN
        var tiltY = Double.NaN
        if (kind == KIND_PEN || kind == KIND_ERASER) {
            val orientation = e.getAxisValue(MotionEvent.AXIS_ORIENTATION, i)
            val tilt = e.getAxisValue(MotionEvent.AXIS_TILT, i)
            if (tilt > 0.05) {
                val tan = kotlin.math.tan(tilt)
                tiltX = kotlin.math.atan(kotlin.math.cos(orientation) / tan).toDouble()
                tiltY = kotlin.math.atan(kotlin.math.sin(orientation) / tan).toDouble()
            }
        }
        val x = if (hist >= 0) e.getHistoricalX(i, hist) else e.getX(i)
        val y = if (hist >= 0) e.getHistoricalY(i, hist) else e.getY(i)
        nativePointer(handle, phase, e.getPointerId(i), x.toDouble(), y.toDouble(), pressure, tiltX, tiltY, kind, tMs * 1000)
    }

    // ── 控制面（Activity/工具栏调用）──

    fun setToolCode(code: Int) {
        currentToolCode = code
        if (handle != 0L) nativeSetTool(handle, code)
    }

    fun drawText(font: ByteArray, text: String, canvasX: Double, canvasY: Double, size: Float): Boolean =
        handle != 0L && nativeDrawText(handle, font, text, canvasX, canvasY, size.toDouble())

    /** 设置文字字体（对象光栅化；启动后调一次）。 */
    fun setTextFont(font: ByteArray) {
        if (handle != 0L) nativeSetTextFont(handle, font)
    }

    /** 新增文字对象（非破坏可编辑）。y 为基线原点画布坐标。 */
    fun addTextObject(text: String, x: Double, y: Double, size: Float): Boolean =
        handle != 0L && nativeAddTextObject(handle, text, x, y, size.toDouble())

    /** 更新最近命中的文字对象。 */
    fun updateTextObject(text: String, size: Float): Boolean =
        handle != 0L && nativeUpdateTextObject(handle, text, size.toDouble())

    /** 删除最近命中的文字对象。 */
    fun deleteTextObject(): Boolean = handle != 0L && nativeDeleteTextObject(handle)

    /** 编辑计数（自动保存脏检查）。 */
    fun editCount(): Long = if (handle == 0L) 0 else nativeEditCount(handle)

    /** 呈现帧计数（单调；监控关闭时冻结）。 */
    fun renderCount(): Long = if (handle == 0L) 0 else nativeRenderCount(handle)

    /** 帧率监控开关。 */
    fun setFpsMonitor(on: Boolean) {
        if (handle != 0L) nativeSetFpsMonitor(handle, on)
    }

    /** 存 .ora 工程字节。 */
    fun saveOra(): ByteArray? = if (handle == 0L) null else nativeSaveOra(handle)

    /** 载入 .ora 替换当前文档。 */
    fun loadOra(data: ByteArray): Boolean = handle != 0L && nativeLoadOra(handle, data)

    fun setToolEraser(eraser: Boolean) = setToolCode(if (eraser) TOOL_ERASER else TOOL_BRUSH)

    fun setBrushSize(size: Float) {
        if (handle != 0L) nativeSetBrushSize(handle, size.toDouble())
    }

    fun setBrushColor(r: Int, g: Int, b: Int) {
        if (handle != 0L) nativeSetBrushColor(handle, r, g, b)
    }

    fun presetNames(): List<String> =
        if (handle == 0L) emptyList() else (nativePresetNames(handle)?.toList() ?: emptyList())

    fun applyPreset(name: String): Boolean = handle != 0L && nativeApplyPreset(handle, name)

    fun undo(): Boolean = handle != 0L && nativeUndo(handle)
    fun redo(): Boolean = handle != 0L && nativeRedo(handle)
    fun addLayer(): Boolean = handle != 0L && nativeAddLayer(handle)
    fun mergeDown(): Boolean = handle != 0L && nativeMergeDown(handle)
    fun flatten(): Boolean = handle != 0L && nativeFlatten(handle)
    fun layerCount(): Int = if (handle == 0L) 0 else nativeLayerCount(handle)
    fun activeLayerIndex(): Int = if (handle == 0L) -1 else nativeActiveLayerIndex(handle)
    fun selectLayerIndex(index: Int): Boolean = handle != 0L && nativeSelectLayerIndex(handle, index)
    fun layerNameAt(index: Int): String? = if (handle == 0L) null else nativeLayerNameAt(handle, index)
    fun fitToContent() {
        if (handle != 0L) nativeFitToContent(handle)
    }

    fun zoom100() {
        if (handle != 0L) nativeZoom100(handle)
    }

    fun setShowGrid(show: Boolean) {
        if (handle != 0L) nativeSetShowGrid(handle, show)
    }

    fun exportPng(): ByteArray? = if (handle == 0L) null else nativeExportPng(handle)
    fun importPng(data: ByteArray): Boolean = handle != 0L && nativeImportPng(handle, data)

    /** 附件：PNG/JPEG/WebP/SVG 自动识别 → 新图层（视野中心）。 */
    fun importImage(data: ByteArray): Boolean = handle != 0L && nativeImportImage(handle, data)

    /** 附件（壳层已渲染的直行 RGBA，如 PDF 页）→ 浮动层拖拽放置。 */
    fun pasteRgba(rgba: ByteArray, w: Int, h: Int): Boolean =
        handle != 0L && nativePasteRgba(handle, rgba, w, h)

    fun isTransforming(): Boolean = handle != 0L && nativeTransforming(handle)
    fun transformRotate(deltaDeg: Double) {
        if (handle != 0L) nativeTransformRotate(handle, deltaDeg)
    }

    fun transformScale(factor: Double) {
        if (handle != 0L) nativeTransformScale(handle, factor)
    }

    fun commitTransform(): Boolean = handle != 0L && nativeCommitTransform(handle)
    fun cancelTransform(): Boolean = handle != 0L && nativeCancelTransform(handle)
}
