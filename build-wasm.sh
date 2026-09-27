#!/usr/bin/env bash
# paintEngine wasm 构建脚本：产出 slim / full / gpu 三个产物
#
# 用法:
#   ./build-wasm.sh          # 构建 full（默认）
#   ./build-wasm.sh slim     # 仅构建 slim
#   ./build-wasm.sh all      # 构建全部三个

set -euo pipefail
cd "$(dirname "$0")"

TARGET="wasm32-unknown-unknown"
WASM_DIR="crates/paint-wasm"
OUT_BASE="$WASM_DIR/www"

build_variant() {
    local name=$1 features=$2
    echo "── 构建 ${name} ──"

    local cargo_args="build --target $TARGET --release -p paint-wasm"
    if [ -n "$features" ]; then
        cargo_args="$cargo_args --features $features"
    else
        cargo_args="$cargo_args --no-default-features"
    fi

    # Step 1: cargo 构建 .wasm
    (cd "$WASM_DIR" && cargo $cargo_args 2>&1 | tail -1)

    local wasm_file="target/$TARGET/release/paint_wasm.wasm"
    if [ ! -f "$wasm_file" ]; then
        echo "错误: wasm 构建产物未找到: $wasm_file"
        exit 1
    fi

    # Step 2: wasm-bindgen 生成 JS 绑定
    local dest="$OUT_BASE/pkg/$name"
    rm -rf "$dest"
    mkdir -p "$dest"
    wasm-bindgen "$wasm_file" --target web --out-dir "$dest" --out-name paint_wasm 2>&1 | tail -1

    # Step 3: 报告体积
    local size=$(stat -f%z "$dest/paint_wasm_bg.wasm" 2>/dev/null || echo 0)
    local size_kb=$((size / 1024))
    printf "  %s: %d KB (%.1f MB)\n" "$name" "$size_kb" "$(echo "scale=1; $size/1048576" | bc)"
}

case "${1:-full}" in
    slim)  build_variant slim "" ;;
    full)  build_variant full "svg,text" ;;
    gpu)   build_variant gpu "svg,text,gpu" ;;
    all)
        build_variant slim ""
        build_variant full "svg,text"
        build_variant gpu "svg,text,gpu"
        ;;
    *)     echo "用法: $0 [slim|full|gpu|all]"; exit 1 ;;
esac

echo ""
echo "产物位置: $OUT_BASE/pkg/<variant>/paint_wasm{.js,_bg.wasm}"
echo "  slim: 最小体积（无 SVG 导入、无文字工具）"
echo "  full: 全功能（SVG + 文字 + 滤镜 + 对称）"
echo "  gpu:  全功能 + GPU 合成（WebGPU/WebGL）"
