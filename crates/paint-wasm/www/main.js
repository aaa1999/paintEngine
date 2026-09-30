// paintEngine Web 演示引导。
// 前置：wasm-pack build crates/paint-wasm --target web --out-dir ../www/pkg
import init, { PaintApp } from "./pkg/paint_wasm.js?v=12";

const $ = (id) => document.getElementById(id);

await init();
const app = new PaintApp($("canvas"));
window.app = app; // 调试/自动化入口
$("status").textContent = "就绪";

// 工具切换
const brushBtn = $("brush");
const eraserBtn = $("eraser");
let shapeFill = false;
const shapeIds = ["line", "rect", "ellipse"];
function shapeName(base) { return shapeFill ? `${base}-fill` : base; }
function selectTool(tool) {
  app.set_tool(tool);
  const shapeBase = shapeIds.includes(tool) ? tool : null;
  for (const id of ["brush", "eraser", ...shapeIds, "textTool", "fillTool"]) {
    const on =
      (id === "brush" && tool === "brush") ||
      (id === "eraser" && tool === "eraser") ||
      (id === "textTool" && tool === "text") ||
      (id === "fillTool" && tool === "fill") ||
      (id === shapeBase);
    $(id).classList.toggle("active", on);
  }
  if (tool === "text") $("status").textContent = "文字：点击画布输入";
}
brushBtn.onclick = () => selectTool("brush");
eraserBtn.onclick = () => selectTool("eraser");
for (const id of shapeIds) $(id).onclick = () => selectTool(id);
$("fillToggle").onclick = () => {
  shapeFill = !shapeFill;
  $("fillToggle").textContent = shapeFill ? "填充" : "描边";
  $("fillToggle").classList.toggle("active", shapeFill);
  const cur = app.tool();
  if (shapeIds.includes(cur)) selectTool(cur);
};
$("textTool").onclick = () => selectTool("text");
$("fillTool").onclick = () => selectTool("fill");
$("mask").onclick = () => {
  const on = app.tool() === "mask";
  app.set_tool(on ? "brush" : "mask");
  $("mask").classList.toggle("active", !on);
};
window.addEventListener("keydown", (e) => {
  // 文字输入中不劫持按键
  if (document.activeElement && document.activeElement.id === "textInput") return;
  if (e.key === "b" || e.key === "B") selectTool("brush");
  if (e.key === "e" || e.key === "E") selectTool("eraser");
  if (e.key === "l" || e.key === "L") selectTool("line");
  if (e.key === "r" || e.key === "R") selectTool("rect");
  if (e.key === "o" || e.key === "O") selectTool("ellipse");
  if (e.key === "t" || e.key === "T") selectTool("text");
  if (e.key === "f" || e.key === "F") selectTool("fill");
  if ((e.ctrlKey || e.metaKey) && e.key === "z") {
    e.shiftKey ? app.redo() : app.undo();
  }
});

// ── 文字工具：点击取锚点 → 浮层输入 → 浏览器字体渲染 → 落墨 ──
const textInput = $("textInput");
canvasEl().addEventListener("pointerup", (e) => {
  if (app.tool() !== "text") return;
  // 先查命中已有对象（编辑/删除），再查新锚点
  const edit = app.take_text_edit();
  if (edit && edit.length >= 3) {
    textInput.hidden = false;
    textInput.value = edit[0];
    textInput.style.left = `${e.clientX + 4}px`;
    textInput.style.top = `${e.clientY - 56}px`;
    textInput.focus();
    textInput._anchor = null;
    textInput._edit = { size: Number(edit[1]) || 48 };
    $("status").textContent = "编辑文字（Enter 更新 · Esc 取消）";
    return;
  }
  textInput._edit = null;
  const anchor = app.take_text_anchor();
  if (anchor && anchor.length >= 2) {
    textInput.hidden = false;
    textInput.value = "";
    textInput.style.left = `${e.clientX + 4}px`;
    textInput.style.top = `${e.clientY - 28}px`;
    textInput.focus();
    textInput._anchor = anchor;
  }
});
function closeTextInput() {
  textInput.hidden = true;
  textInput._anchor = null;
  textInput.blur();
}
textInput.addEventListener("keydown", (e) => {
  if (e.key === "Escape") { closeTextInput(); return; }
  if (e.key === "Delete" && textInput._edit) {
    app.delete_text_object();
    closeTextInput();
    $("status").textContent = "已删除文字";
    return;
  }
  if (e.key !== "Enter") return;
  const text = textInput.value;
  const anchor = textInput._anchor;
  const edit = textInput._edit;
  closeTextInput();
  if (!text) return;
  const size = edit
    ? Math.max(12, Math.min(200, edit.size))
    : Math.max(12, Math.min(200, Number($("size").value) * 4));
  const color = $("color").value;
  // 离屏 canvas 用系统字体光栅化（中文原生支持），getImageData 直行 RGBA
  const c = document.createElement("canvas");
  const font = `${size}px sans-serif`;
  const m0 = c.getContext("2d");
  m0.font = font;
  const w = Math.max(1, Math.ceil(m0.measureText(text).width));
  const h = Math.ceil(size * 1.5);
  c.width = w; c.height = h;
  const ctx = c.getContext("2d");
  ctx.font = font;
  ctx.fillStyle = color;
  ctx.textBaseline = "alphabetic";
  ctx.fillText(text, 0, size);
  const img = ctx.getImageData(0, 0, w, h);
  const rgba = new Uint8Array(img.data.buffer);
  let ok;
  if (edit) {
    ok = app.update_text_object(text, size, rgba, w, h, 0n, BigInt(-Math.round(size)));
  } else {
    if (!anchor) return;
    // 锚点=点击处视觉左上角：基线原点 = anchor + 0.8*size；光栅顶在基线上方 size
    ok = app.add_text_object(text, size, anchor[0], anchor[1] + size * 0.6, rgba, w, h, 0n, BigInt(-Math.round(size)));
  }
  $("status").textContent = ok
    ? (edit ? `已更新文字 "${text}"` : `已插入文字 "${text}"`)
    : "文字操作失败";
});
textInput.addEventListener("blur", () => { if (!textInput.hidden) closeTextInput(); });

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

// 笔刷预设
function refreshPresets() {
  const sel = $("preset");
  const cur = app.current_preset_name();
  sel.innerHTML = "";
  for (const n of app.preset_names()) {
    sel.add(new Option(n, n, false, n === cur));
  }
}
$("preset").onchange = () => {
  app.apply_preset($("preset").value);
  persistPresets();
};
$("savePreset").onclick = () => {
  const name = prompt("预设名称：", app.current_preset_name() || "我的笔");
  if (!name) return;
  if (app.save_preset(name)) { refreshPresets(); persistPresets(); $("status").textContent = `已保存预设 ${name}`; }
};
function persistPresets() {
  try { localStorage.setItem("paintPresets", app.export_presets()); } catch (e) {}
}
try {
  const saved = localStorage.getItem("paintPresets");
  if (saved) app.import_presets(saved);
} catch (e) {}
refreshPresets();

// 对称绘画
$("sym").onclick = () => {
  const name = app.cycle_symmetry();
  $("sym").classList.toggle("active", name !== "关");
  $("status").textContent = `对称: ${name}`;
};
window.addEventListener("keydown", (e) => {
  if (e.key === "x" || e.key === "X") { e.preventDefault(); $("sym").click(); }
});

// 画布尺寸
$("canvasPreset").onchange = () => {
  const v = $("canvasPreset").value;
  if (!v) { app.clear_canvas(); $("status").textContent = "画布: 无限"; }
  else {
    const [w, h] = v.split(",").map(Number);
    app.set_canvas(w, h);
    $("status").textContent = `画布: ${w}×${h}`;
  }
};

// 滤镜
$("fxInvert").onclick = () => { if (app.apply_invert()) $("status").textContent = "已反色"; };
$("fxGray").onclick = () => { if (app.apply_grayscale()) $("status").textContent = "已灰度"; };
$("fxBlur").onclick = () => { if (app.apply_blur(8)) $("status").textContent = "已模糊 r=8"; };

// 多文档标签栏
function refreshTabs() {
  const bar = document.getElementById("tabBar");
  bar.innerHTML = "";
  const n = app.doc_count();
  for (let i = 0; i < n; i++) {
    const name = i === n - 1 ? "当前" : app.doc_name(i);
    const tab = document.createElement("span");
    tab.className = "tab" + (i === n - 1 ? " active" : "");
    tab.textContent = name;
    tab.onclick = () => { if (i < n - 1) { app.doc_switch(i); refreshTabs(); refreshLayers(); } };
    bar.appendChild(tab);
  }
  const plus = document.createElement("span");
  plus.className = "tab";
  plus.textContent = "＋";
  plus.onclick = () => { app.doc_new(); refreshTabs(); refreshLayers(); };
  bar.appendChild(plus);
}
refreshTabs();

// 插件滤镜
$("fxPlugin").onclick = () => {
  // 示例：R+30 B-20 通道偏移
  const params = JSON.stringify({ r: 30, g: 0, b: -20 });
  if (app.plugin_apply_filter("channel_shift", params)) {
    $("status").textContent = "已应用插件滤镜: 通道偏移";
  } else {
    $("status").textContent = "插件滤镜不可用";
  }
};

// 调整图层（非破坏性）
$("adjToggle").onclick = () => {
  $("adjBar").hidden = !$("adjBar").hidden;
  $("adjToggle").classList.toggle("active", !$("adjBar").hidden);
};
function applyAdj() {
  const b = Number($("adjB").value), c = Number($("adjC").value), s = Number($("adjS").value);
  $("adjBLabel").textContent = `B${b}`; $("adjCLabel").textContent = `C${c}`; $("adjSLabel").textContent = `S${s}`;
  app.set_layer_adjustment(b, c, s, 0, 1);
}
$("adjB").oninput = applyAdj;
$("adjC").oninput = applyAdj;
$("adjS").oninput = applyAdj;
$("adjClear").onclick = () => {
  $("adjB").value = 0; $("adjC").value = 0; $("adjS").value = 0;
  app.clear_layer_adjustment();
  applyAdj();
};

// 图层面板
const BLEND_NAMES = app.blend_mode_names();
function refreshLayers() {
  const infos = app.layer_infos();
  const active = app.active_layer_id();
  const list = $("layerList");
  list.innerHTML = "";
  $("layerCount").textContent = `(${infos.length})`;
  // 自顶向下（数组底→顶，反转显示）
  for (let i = infos.length - 1; i >= 0; i--) {
    const li = infos[i];
    const row = document.createElement("div");
    row.className = "layerRow" + (li.id === active ? " active" : "");
    row.dataset.index = i;
    // 眼
    const eye = document.createElement("span");
    eye.className = "eye";
    eye.textContent = li.visible ? "👁" : "🚫";
    eye.onclick = (e) => { e.stopPropagation(); app.set_layer_visible_by_index(i, !li.visible); refreshLayers(); };
    row.appendChild(eye);
    // 名
    const name = document.createElement("span");
    name.className = "lName";
    name.textContent = (li.group ? "  └ " : "") + li.name + (li.clipped ? " ⧉" : "") + (li.hasMask ? " ◐" : "");
    if (li.group) { row.style.paddingLeft = "18px"; }
    row.appendChild(name);
    // 重排/删除
    const ops = document.createElement("span");
    ops.className = "lOps";
    if (i < infos.length - 1) {
      const up = document.createElement("button");
      up.textContent = "↑"; up.title = "上移";
      up.onclick = (e) => { e.stopPropagation(); app.reorder_layer_by_index(i, i + 1); refreshLayers(); };
      ops.appendChild(up);
    }
    if (i > 0) {
      const dn = document.createElement("button");
      dn.textContent = "↓"; dn.title = "下移";
      dn.onclick = (e) => { e.stopPropagation(); app.reorder_layer_by_index(i, i - 1); refreshLayers(); };
      ops.appendChild(dn);
    }
    row.appendChild(ops);
    // 选层
    row.onclick = () => { app.select_layer_by_id(li.id); refreshLayers(); };
    list.appendChild(row);
    // 活动层的属性行（透明度/混合模式）
    if (li.id === active) {
      const det = document.createElement("div");
      det.className = "layerDetail";
      det.style.marginLeft = "10px";
      // 透明度
      const opLabel = document.createElement("span");
      opLabel.textContent = "透";
      det.appendChild(opLabel);
      const opRange = document.createElement("input");
      opRange.type = "range"; opRange.min = 0; opRange.max = 100;
      opRange.value = Math.round(li.opacity * 100);
      opRange.oninput = () => app.set_layer_opacity_by_index(i, opRange.value / 100);
      det.appendChild(opRange);
      const opVal = document.createElement("span");
      opVal.textContent = `${Math.round(li.opacity * 100)}%`;
      opRange.oninput = () => { app.set_layer_opacity_by_index(i, opRange.value / 100); opVal.textContent = `${opRange.value}%`; };
      det.appendChild(opVal);
      // 混合模式
      const blendSel = document.createElement("select");
      for (const [bi, bn] of BLEND_NAMES.entries()) blendSel.add(new Option(bn, bi));
      blendSel.value = BLEND_NAMES.indexOf(li.blendMode) >= 0 ? BLEND_NAMES.indexOf(li.blendMode) : 0;
      blendSel.onchange = () => app.set_layer_blend_by_index(i, Number(blendSel.value));
      det.appendChild(blendSel);
      list.appendChild(det);
    }
  }
}
$("lyAdd").onclick = () => { app.add_layer(); refreshLayers(); };
$("lyDup").onclick = () => {
  const idx = app.layer_infos().findIndex((l) => l.id === app.active_layer_id());
  if (idx >= 0) { app.duplicate_layer_by_index(idx); refreshLayers(); }
};
$("lyDel").onclick = () => {
  const idx = app.layer_infos().findIndex((l) => l.id === app.active_layer_id());
  if (idx >= 0 && app.layer_infos().length > 1) { app.remove_layer_by_index(idx); refreshLayers(); }
};
$("lyMerge").onclick = () => { app.merge_down(); refreshLayers(); };
$("lyFlat").onclick = () => { app.flatten(); refreshLayers(); };
// 面板操作后刷新（笔刷操作改变层数时也刷新）
const _origRefreshXbar = refreshXbar;
refreshXbar = () => { _origRefreshXbar(); refreshLayers(); };
refreshLayers();

// SVG 导入
$("importSvg").onclick = () => $("svgFile").click();
$("svgFile").onchange = async (e) => {
  const f = e.target.files[0];
  if (!f) return;
  const text = await f.text();
  const r = app.import_svg(new TextEncoder().encode(text), 1.0);
  if (r >= 0) {
    $("status").textContent = `已导入 SVG: ${f.name}`;
    refreshLayers();
  } else {
    $("status").textContent = "SVG 解析失败";
  }
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

$("exportJpg").onclick = () => {
  const bytes = app.export_jpeg(92);
  if (!bytes.length) { $("status").textContent = "画布为空"; return; }
  const blob = new Blob([new Uint8Array(bytes)], { type: "image/jpeg" });
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = "painting.jpg";
  a.click();
  URL.revokeObjectURL(a.href);
  $("status").textContent = "已导出 JPG";
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


// ── 自动保存（IndexedDB）──
const IDB = {
  db: null,
  async open() {
    if (this.db) return this.db;
    this.db = await new Promise((res, rej) => {
      const r = indexedDB.open("paintengine", 1);
      r.onupgradeneeded = () => r.result.createObjectStore("kv");
      r.onsuccess = () => res(r.result);
      r.onerror = () => rej(r.error);
    });
    return this.db;
  },
  async put(key, val) {
    const db = await this.open();
    return new Promise((res, rej) => {
      const tx = db.transaction("kv", "readwrite");
      tx.objectStore("kv").put(val, key);
      tx.oncomplete = () => res(true);
      tx.onerror = () => rej(tx.error);
    });
  },
  async get(key) {
    const db = await this.open();
    return new Promise((res) => {
      const tx = db.transaction("kv", "readonly");
      const rq = tx.objectStore("kv").get(key);
      rq.onsuccess = () => res(rq.result);
      rq.onerror = () => res(undefined);
    });
  },
};

let savedEditCount = 0;
async function autosave() {
  try {
    if (app.edit_count() === savedEditCount) return;
    const bytes = app.export_ora();
    if (!bytes) return;
    await IDB.put("autosave", bytes);
    savedEditCount = app.edit_count();
  } catch (e) { console.warn("自动保存失败", e); }
}
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") autosave();
});
window.addEventListener("beforeunload", () => autosave());
setInterval(autosave, 30000);

// 启动恢复
(async () => {
  try {
    const bytes = await IDB.get("autosave");
    console.log("[autosave-restore]", bytes ? bytes.byteLength : "none"); localStorage.setItem("__asdbg", bytes ? String(bytes.byteLength) : "none");
    if (bytes && bytes.byteLength > 100) {
      if (app.import_ora(bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes))) {
        savedEditCount = app.edit_count();
        $("status").textContent = "已恢复上次会话（自动保存）";
        console.log("[autosave-restore] ok"); localStorage.setItem("__asdbg", localStorage.getItem("__asdbg") + ":ok");
      } else {
        console.log("[autosave-restore] import failed");
      }
    }
  } catch (e) { console.warn("[autosave-restore] ERR", e); localStorage.setItem("__asdbg", "ERR:" + e.message); }
})();

// ── 帧率监控（配置：点击悬浮框开关，localStorage 持久化，默认开）──
const fpsBox = document.createElement("div");
fpsBox.id = "fpsBox";
fpsBox.title = "点击开关帧率监控";
Object.assign(fpsBox.style, {
  position: "fixed", left: "8px", top: "52px", zIndex: 20,
  background: "rgba(0,0,0,0.6)", color: "#7fe388",
  font: "12px/1.6 ui-monospace,monospace", padding: "2px 8px",
  borderRadius: "4px", cursor: "pointer", userSelect: "none",
});
fpsBox.textContent = "FPS --";
document.body.appendChild(fpsBox);
let fpsOn = localStorage.getItem("pe_fps_monitor") !== "0";
app.set_fps_monitor(fpsOn);
// 帧率 = 呈现计数差值 / 采样间隔（引擎不依赖平台时钟，wasm 无 Instant）
let fpsLastCount = 0;
let fpsLastTime = performance.now();
let fpsSmoothed = 0;
function refreshFpsBox() {
  if (!fpsOn) {
    fpsBox.textContent = "FPS 关";
    return;
  }
  const now = performance.now();
  const c = app.render_count();
  const dt = (now - fpsLastTime) / 1000;
  if (dt > 0.2) {
    const inst = (c - fpsLastCount) / dt;
    fpsSmoothed = fpsSmoothed === 0 ? inst : fpsSmoothed * 0.6 + inst * 0.4;
    fpsLastCount = c;
    fpsLastTime = now;
  }
  fpsBox.textContent = `FPS ${fpsSmoothed.toFixed(1)}`;
}
fpsBox.onclick = () => {
  fpsOn = !fpsOn;
  localStorage.setItem("pe_fps_monitor", fpsOn ? "1" : "0");
  app.set_fps_monitor(fpsOn);
  if (fpsOn) { fpsLastCount = 0; fpsLastTime = performance.now(); fpsSmoothed = 0; }
  refreshFpsBox();
};
setInterval(refreshFpsBox, 500);
refreshFpsBox();
