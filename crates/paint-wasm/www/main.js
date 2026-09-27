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

// 导入导出
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
