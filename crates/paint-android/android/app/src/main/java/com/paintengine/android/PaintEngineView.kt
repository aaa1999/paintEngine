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
    }

    private external fun nativeCreate(): Long
    private external fun nativeDestroy(handle: Long)
    private external fun nativeResize(handle: Long, w: Int, h: Int, scale: Float)
    private external fun nativePointer(
        handle: Long, phase: Int, id: Int,
        x: Double, y: Double, pressure: Double, kind: Int, tUs: Long,
    )
    private external fun nativePenInRange(handle: Long, inRange: Boolean)
    private external fun nativeFocus(handle: Long, focused: Boolean)
    private external fun nativeRender(handle: Long, bitmap: Bitmap): Boolean
    private external fun nativeSetTool(handle: Long, eraser: Boolean)
    private external fun nativeSetBrushSize(handle: Long, size: Double)
    private external fun nativeSetBrushColor(handle: Long, r: Int, g: Int, b: Int)
    private external fun nativeUndo(handle: Long): Boolean
    private external fun nativeRedo(handle: Long): Boolean
    private external fun nativeAddLayer(handle: Long): Boolean
    private external fun nativeMergeDown(handle: Long): Boolean
    private external fun nativeFlatten(handle: Long): Boolean
    private external fun nativeFitToContent(handle: Long)
    private external fun nativeZoom100(handle: Long)
    private external fun nativeSetShowGrid(handle: Long, show: Boolean)
    private external fun nativeExportPng(handle: Long): ByteArray?
    private external fun nativeImportPng(handle: Long, data: ByteArray): Boolean

    private var handle: Long = 0L
    private var bitmap: Bitmap? = null

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
        val x = if (hist >= 0) e.getHistoricalX(i, hist) else e.getX(i)
        val y = if (hist >= 0) e.getHistoricalY(i, hist) else e.getY(i)
        nativePointer(handle, phase, e.getPointerId(i), x.toDouble(), y.toDouble(), pressure, kind, tMs * 1000)
    }

    // ── 控制面（Activity/工具栏调用）──

    fun setToolEraser(eraser: Boolean) = nativeSetTool(handle, eraser)
    fun setBrushSize(size: Float) = nativeSetBrushSize(handle, size.toDouble())
    fun setBrushColor(r: Int, g: Int, b: Int) = nativeSetBrushColor(handle, r, g, b)
    fun undo(): Boolean = nativeUndo(handle)
    fun redo(): Boolean = nativeRedo(handle)
    fun addLayer(): Boolean = nativeAddLayer(handle)
    fun mergeDown(): Boolean = nativeMergeDown(handle)
    fun flatten(): Boolean = nativeFlatten(handle)
    fun fitToContent() = nativeFitToContent(handle)
    fun zoom100() = nativeZoom100(handle)
    fun setShowGrid(show: Boolean) = nativeSetShowGrid(handle, show)
    fun exportPng(): ByteArray? = nativeExportPng(handle)
    fun importPng(data: ByteArray): Boolean = nativeImportPng(handle, data)
}
