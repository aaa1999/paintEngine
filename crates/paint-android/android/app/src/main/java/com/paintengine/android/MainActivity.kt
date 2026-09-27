package com.paintengine.android

import android.graphics.Color
import android.net.Uri
import android.os.Bundle
import android.provider.MediaStore
import android.widget.Button
import android.widget.LinearLayout
import android.widget.Toast
import java.io.OutputStream

/**
 * paintEngine Android 演示 Activity。
 * 顶部画布 + 底部工具栏：画笔/橡皮/撤销/重做/存 PNG。
 */
class MainActivity : android.app.Activity() {

    private lateinit var paintView: PaintEngineView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        paintView = PaintEngineView(this)
        paintView.setBrushSize(18f)

        val toolbar = LinearLayout(this).apply {
            orientation = LinearLayout.HORIZONTAL
            setBackgroundColor(Color.parseColor("#F3F3F3"))
        }
        fun button(label: String, onClick: () -> Unit) = Button(this).apply {
            text = label
            setOnClickListener { onClick() }
            layoutParams = LinearLayout.LayoutParams(
                0, LinearLayout.LayoutParams.WRAP_CONTENT, 1f,
            )
        }

        toolbar.addView(button("画笔") { paintView.setToolEraser(false) })
        toolbar.addView(button("橡皮") { paintView.setToolEraser(true) })
        toolbar.addView(button("撤销") { paintView.undo(); paintView.invalidate() })
        toolbar.addView(button("重做") { paintView.redo(); paintView.invalidate() })
        toolbar.addView(button("图层") { showLayerDialog() })
        toolbar.addView(button("适配") { paintView.fitToContent(); paintView.invalidate() })
        // 注：图层按钮移到底部 dialogs，见 showLayerDialog
        toolbar.addView(button("保存") { savePng() })

        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
        }
        root.addView(
            paintView,
            LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT, 0, 1f,
            ),
        )
        root.addView(
            toolbar,
            LinearLayout.LayoutParams(
                LinearLayout.LayoutParams.MATCH_PARENT,
                LinearLayout.LayoutParams.WRAP_CONTENT,
            ),
        )
        setContentView(root)
    }

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
        // 栈位 0=底层；列表显示自顶向下
        val indices = (0 until count).reversed().toList()
        val active = paintView.activeLayerIndex()
        val checkedPos = indices.indexOf(active).coerceAtLeast(0)
        val labels = indices.mapIndexed { pos, idx ->
            val mark = if (idx == active) "● " else "○ "
            val name = paintView.layerNameAt(idx) ?: ("图层 ${idx + 1}")
            mark + name
        }.toTypedArray()

        android.app.AlertDialog.Builder(this)
            .setTitle("图层（共 $count 层）")
            .setSingleChoiceItems(labels, checkedPos) { dlg, which ->
                val idx = indices[which]
                paintView.selectLayerIndex(idx)
                paintView.invalidate()
                dlg.dismiss()
                Toast.makeText(this, "当前层：${paintView.layerNameAt(idx) ?: (idx + 1)}", Toast.LENGTH_SHORT).show()
            }
            .setPositiveButton("新建") { _, _ ->
                paintView.addLayer()
                paintView.invalidate()
                Toast.makeText(this, "已新建图层（共 ${paintView.layerCount()} 层），之后画在新层上", Toast.LENGTH_SHORT).show()
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
}
