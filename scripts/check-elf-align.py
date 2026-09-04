#!/usr/bin/env python3
"""check-elf-align.py — ELF LOAD 段对齐检测（Android 15+ 16KB 页要求）

用法: python3 scripts/check-elf-align.py <lib.so|lib*.dylib>
退出码 0 = 兼容（所有 PT_LOAD p_align ≤ 0x1000，或均为 16384 的倍数）
退出码 1 = 不满足 16KB 真机要求
"""
import struct
import sys


def main(path: str) -> int:
    with open(path, "rb") as f:
        data = f.read(64)
        if data[:4] != b"\x7fELF":
            print(f"{path}: not an ELF")
            return 1
        is64 = data[4] == 2
        if not is64:
            print(f"{path}: 32-bit ELF (armv7) — 4KB 页即可")
            return 0
        f.seek(0)
        ident = f.read(16)
        e_phoff = struct.unpack_from("<Q", f.read(8), 0)[0] if False else None
        # 重读 ELF64 头
        f.seek(0)
        header = f.read(64)
        e_phoff, e_phentsize, e_phnum = struct.unpack_from("<QHH", header, 32)
        f.seek(e_phoff)
        loads = []
        for _ in range(e_phnum):
            ph = f.read(e_phentsize)
            p_type, p_flags = struct.unpack_from("<II", ph, 0)
            p_align = struct.unpack_from("<Q", ph, 48)[0]
            if p_type == 1:  # PT_LOAD
                loads.append(p_align)
        if not loads:
            print(f"{path}: no PT_LOAD segments")
            return 1
        ok16k = all(a % 16384 == 0 for a in loads if a > 4096) or all(
            a <= 4096 for a in loads
        )
        for a in loads:
            print(f"  PT_LOAD p_align = 0x{a:x}")
        if ok16k:
            print(f"{path}: OK for 16KB-page devices")
            return 0
        print(f"{path}: NOT 16KB-aligned — rebuild with NDK r28+ (llvm -z max-page-size=16384)")
        return 1


if __name__ == "__main__":
    if len(sys.argv) != 2:
        print(__doc__)
        sys.exit(2)
    sys.exit(main(sys.argv[1]))
