#!/usr/bin/env python3
"""跨语言核对:Kotlin `external fun` 声明 ↔ Rust RegisterNatives 注册表。

手写 JNI 描述符没有编译期保障(两侧语言各自为政),错一个字符就是
真机 UnsatisfiedLinkError。本脚本机械比对两侧,CI/提交前跑一次:

    python3 crates/paint-android/check_jni_sigs.py
"""

import os
import re
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
KT = os.path.join(HERE, 'android/library/src/main/java/com/paintengine/android/PaintEngineView.kt')
RS = os.path.join(HERE, 'src/lib.rs')

TY = {
    'Long': 'J', 'Int': 'I', 'Float': 'F', 'Double': 'D', 'Boolean': 'Z',
    'ByteArray': '[B', 'String': 'Ljava/lang/String;',
    'Bitmap': 'Landroid/graphics/Bitmap;',
}
RET = dict(TY, **{
    'Unit': 'V', 'FloatArray': '[F', 'Array<String>': '[Ljava/lang/String;',
})


def kotlin_sigs():
    kt = open(KT).read()
    kt = re.sub(r'/\*.*?\*/', '', kt, flags=re.S)
    fns = re.findall(
        r'external fun\s+(\w+)\s*\((.*?)\)\s*(?::\s*([\w?<>\[\]]+))?\s*[=\s{]',
        kt, flags=re.S,
    )
    sigs = {}
    for name, params, ret in fns:
        parts = [p.strip() for p in params.split(',') if p.strip()]
        sig = '(' + ''.join(TY[p.split(':')[1].strip()] for p in parts) + ')'
        sig += RET[ret.strip().rstrip('?')] if ret else 'V'
        sigs[name] = sig
    return sigs


def rust_sigs():
    rs = open(RS).read()
    return dict(re.findall(r'entry\(\s*"(\w+)",\s*"(\([^)]*\)\S*)"', rs))


def main() -> int:
    kt_sigs, rs_sigs = kotlin_sigs(), rust_sigs()
    print(f'Kotlin {len(kt_sigs)} 个声明 / Rust {len(rs_sigs)} 条注册')
    bad = 0
    for name in sorted(set(kt_sigs) | set(rs_sigs)):
        k, r = kt_sigs.get(name), rs_sigs.get(name)
        if k is None:
            print(f'✗ Rust 有 Kotlin 无: {name} {r}'); bad += 1
        elif r is None:
            print(f'✗ Kotlin 有 Rust 无: {name} {k}'); bad += 1
        elif k != r:
            print(f'✗ 签名不一致 {name}: Kotlin={k} Rust={r}'); bad += 1
    if bad:
        print(f'{bad} 处不一致 ✗'); return 1
    print('全部一致 ✓'); return 0


if __name__ == '__main__':
    sys.exit(main())
