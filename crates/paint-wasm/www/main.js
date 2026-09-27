// paintEngine Web 演示引导。
// 前置：wasm-pack build crates/paint-wasm --target web --out-dir ../www/pkg
import init, { PaintApp } from "./pkg/paint_wasm.js";

const $ = (id) => document.getElementById(id);

await init();
const app = new PaintApp($("canvas"));
$("status").textContent = "就绪";

// 工具切换
const brushBtn = $("brush");
const eraserBtn = $("eraser");
function selectTool(tool) {
  app.set_tool(tool);
  brushBtn.classList.toggle("active", tool === "brush");
  eraserBtn.classList.toggle("active", tool === "eraser");
}
brushBtn.onclick = () => selectTool("brush");
eraserBtn.onclick = () => selectTool("eraser");
$("mask").onclick = () => {
  const on = app.tool() === "mask";
  app.set_tool(on ? "brush" : "mask");
  $("mask").classList.toggle("active", !on);
};
window.addEventListener("keydown", (e) => {
  if (e.key === "b" || e.key === "B") selectTool("brush");
  if (e.key === "e" || e.key === "E") selectTool("eraser");
  if ((e.ctrlKey || e.metaKey) && e.key === "z") {
    e.shiftKey ? app.redo() : app.undo();
  }
});

// 笔刷参数
$("color").oninput = (e) => {
  const hex = e.target.value;
  app.set_brush_color(
    parseInt(hex.slice(1, 3), 16),
    parseInt(hex.slice(3, 5), 16),
    parseInt(hex.slice(5, 7), 16)
  );
};
$("size").oninput = (e) => {
  app.set_brush_size(Number(e.target.value));
  $("sizeLabel").textContent = `${e.target.value}px`;
};
$("stab").oninput = (e) => {
  const v = Number(e.target.value);
  app.set_stabilizer(v / 100);
  $("stabLabel").textContent = `稳定${v}%`;
};
$("tilt").oninput = (e) => {
  const v = Number(e.target.value);
  app.set_tilt_sensitivity(v / 100);
  $("tiltLabel").textContent = `倾斜${v}%`;
};
// Alt+点击 = 吸管取色
canvasEl().addEventListener("pointerdown", (e) => {
  if (!e.altKey) return;
  const c = app.pick_color(Math.round(e.clientX * devicePixelRatio), Math.round(e.clientY * devicePixelRatio));
  if (c && c.length === 3) {
    app.set_brush_color(c[0], c[1], c[2]);
    const hex = "#" + c.map((v) => v.toString(16).padStart(2, "0")).join("");
    document.getElementById("color").value = hex;
    $("status").textContent = `取色 ${hex}`;
  }
});
function canvasEl() { return document.getElementById("canvas"); }

// 混合模式
const blendSel = $("blend");
for (const [i, name] of app.blend_mode_names().entries()) {
  blendSel.add(new Option(name, i));
}
blendSel.onchange = () => app.set_active_blend_mode(Number(blendSel.value));

// 图层
$("addLayer").onclick = () => {
  app.add_layer();
  $("status").textContent = `图层：${app.layer_count()}`;
};
$("mergeDown").onclick = () => app.merge_down();
$("flatten").onclick = () => app.flatten();

// 历史
$("undo").onclick = () => app.undo();
$("redo").onclick = () => app.redo();

// 纹理笔刷尖
$("tip").onclick = () => {
  if (app._tipOn) { app.set_brush_tip(new Uint8Array(0)); app._tipOn = false; $("tip").classList.remove("active"); return; }
  $("tipFile").click();
};
$("tipFile").onchange = async (e) => {
  const f = e.target.files[0];
  if (!f) return;
  const data = new Uint8Array(await f.arrayBuffer());
  if (app.set_brush_tip(data)) { app._tipOn = true; $("tip").classList.add("active"); $("status").textContent = `笔尖: ${f.name}`; }
  e.target.value = "";
};

// 剪贴板
$("copyBtn").onclick = async () => {
  if (!app.copy_selection()) { $("status").textContent = "没有可复制内容"; return; }
  $("status").textContent = "已复制";
  // 尝试写系统剪贴板（需安全上下文与用户手势）
  try {
    const png = app.copy_selection_png();
    if (png.length) {
      const blob = new Blob([new Uint8Array(png)], { type: "image/png" });
      await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
      $("status").textContent = "已复制（含系统剪贴板）";
    }
  } catch (e) { /* 内部剪贴板仍可用 */ }
};
$("pasteBtn").onclick = () => {
  if (!app.paste_float()) { $("status").textContent = "剪贴板为空"; return; }
  $("status").textContent = "已粘贴（拖拽定位，Enter 提交）";
  refreshXbar();
};
// Ctrl+V：优先读系统剪贴板图像
window.addEventListener("paste", async (e) => {
  const items = e.clipboardData?.items || [];
  for (const it of items) {
    if (it.type.startsWith("image/")) {
      const blob = it.getAsFile();
      const bytes = new Uint8Array(await blob.arrayBuffer());
      // 非 PNG 也尝试按 PNG 解码失败则忽略
      if (app.paste_image_float(bytes)) {
        $("status").textContent = `已粘贴图像 ${blob.name || ""}`;
        refreshXbar();
        e.preventDefault();
        return;
      }
    }
  }
});
window.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === "c") { e.preventDefault(); $("copyBtn").click(); }
  if ((e.ctrlKey || e.metaKey) && e.key === "v") { e.preventDefault(); $("pasteBtn").click(); }
});

// 内容级变换
let xforming = false;
function refreshXbar() {
  xforming = app.transforming();
  $("xbar").hidden = !xforming;
  $("xform").classList.toggle("active", xforming);
}
$("xform").onclick = () => {
  if (app.transforming()) app.commit_transform();
  else app.begin_transform();
  refreshXbar();
};
$("xRotL").onclick = () => app.transform_rotate(-Math.PI / 12);
$("xRotR").onclick = () => app.transform_rotate(Math.PI / 12);
$("xScaleUp").onclick = () => app.transform_scale(1.1);
$("xScaleDn").onclick = () => app.transform_scale(1 / 1.1);
$("xOk").onclick = () => { app.commit_transform(); refreshXbar(); };
$("xCancel").onclick = () => { app.cancel_transform(); refreshXbar(); };
// 变换中：拖拽移动 / 滚轮旋转（Shift+滚轮缩放）
let xDrag = null;
const cv = document.getElementById("canvas");
cv.addEventListener("pointerdown", (e) => {
  if (!app.transforming()) return;
  xDrag = [e.clientX, e.clientY];
  e.preventDefault();
});
cv.addEventListener("pointermove", (e) => {
  if (!xDrag) return;
  const [px, py, zoom, rot, flip] = app.viewport_params();
  const dx = (e.clientX - xDrag[0]) * devicePixelRatio;
  const dy = (e.clientY - xDrag[1]) * devicePixelRatio;
  xDrag = [e.clientX, e.clientY];
  // 屏幕位移 → 画布位移（含旋转/翻转）
  const c = Math.cos(rot), sn = Math.sin(rot);
  const rx = c * dx + sn * dy;
  const ry = -sn * dx + c * dy;
  const fx = flip > 0.5 ? -rx : rx;
  app.transform_translate(fx / zoom, ry / zoom);
});
cv.addEventListener("pointerup", () => { xDrag = null; });
window.addEventListener("keydown", (e) => {
  if (e.key === "Enter" && app.transforming()) { app.commit_transform(); refreshXbar(); }
  if (e.key === "Escape" && app.transforming()) { app.cancel_transform(); refreshXbar(); }
  if ((e.ctrlKey || e.metaKey) && e.key === "t") {
    e.preventDefault();
    $("xform").click();
  }
});

// 无限画布导航
$("fit").onclick = () => app.fit_to_content();
$("grid").onclick = () => {
  gridOn = !gridOn;
  app.set_show_grid(gridOn);
  $("grid").classList.toggle("active", gridOn);
};
let gridOn = true;
window.addEventListener("keydown", (e) => {
  if ((e.ctrlKey || e.metaKey) && e.key === "0") { e.preventDefault(); app.fit_to_content(); }
  if ((e.ctrlKey || e.metaKey) && e.key === "1") { e.preventDefault(); app.zoom_100(); }
  if (e.key === "g" || e.key === "G") $("grid").click();
});

// 导入导出
// 工程存档（.ora，保留图层/混合模式/不透明度）
$("saveOra").onclick = () => {
  const bytes = app.export_ora();
  if (!bytes.length) { $("status").textContent = "画布为空"; return; }
  const blob = new Blob([new Uint8Array(bytes)], { type: "image/openraster" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = "painting.ora";
  a.click();
  URL.revokeObjectURL(a.href);
  $("status").textContent = "已保存工程";
};
$("openOra").onclick = () => $("oraFile").click();
$("oraFile").onchange = async (e) => {
  const f = e.target.files[0];
  if (!f) return;
  const data = new Uint8Array(await f.arrayBuffer());
  if (app.import_ora(data)) {
    $("status").textContent = `已打开工程（${app.layer_count()} 层）`;
  } else {
    $("status").textContent = "工程解析失败";
  }
  e.target.value = "";
};

$("export").onclick = () => {
  const bytes = app.export_png();
  const blob = new Blob([new Uint8Array(bytes)], { type: "image/png" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = "painting.png";
  a.click();
  URL.revokeObjectURL(a.href);
};
$("import").onchange = async (e) => {
  const file = e.target.files[0];
  if (!file) return;
  const data = new Uint8Array(await file.arrayBuffer());
  if (app.import_png(data)) {
    $("status").textContent = `已导入 ${file.name}`;
  }
};
