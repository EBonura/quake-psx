#!/usr/bin/env python3
"""Execute the actual inline MIPS liquid warp against an independent scalar oracle.

This needs GNU MIPS binutils and a headless PSoXide frontend. It uses synthetic
inputs only, creates no retail assets, and never builds diagnostic code into Quake.
Example: python3 tools/test-liquid-mips.py --frontend /path/to/frontend --out /tmp/liquid-check
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess


def immediate(op, rs, rt, value):
    return op << 26 | rs << 21 | rt << 16 | value & 65535


def harness(routine, source, offsets):
    # Copy the phase window to scratchpad, call the real routine, then write a
    # completion marker. Independent instructions satisfy each stub load delay.
    ins = [
        immediate(15, 0, 4, 0x8010), immediate(15, 0, 5, 0x8010),
        immediate(13, 5, 5, 0x2000), immediate(15, 0, 8, 0x8010),
        immediate(13, 8, 8, 0x1000), immediate(15, 0, 9, 0x1F80),
        immediate(9, 0, 10, 64), immediate(36, 8, 11, 0),
        immediate(9, 8, 8, 1), immediate(40, 9, 11, 0),
        immediate(9, 9, 9, 1), immediate(9, 10, 10, -1),
        immediate(5, 10, 0, -6), 0, immediate(9, 0, 12, 0x55),
        immediate(9, 0, 14, 0xAA), immediate(15, 0, 6, 0x1F80),
        3 << 26 | (0x80020000 >> 2 & 0x3FFFFFF), 0,
        immediate(15, 0, 8, 0x8010), immediate(9, 0, 9, 0x1234),
        immediate(43, 8, 9, 0x3000), immediate(4, 0, 0, -1), 0,
    ]
    payload = bytearray(0xF4000)
    payload[:len(ins) * 4] = struct.pack('<' + 'I' * len(ins), *ins)
    payload[0x10000:0x10000 + len(routine)] = routine
    payload[0xF0000:0xF1000] = source
    payload[0xF1000:0xF1040] = offsets
    header = bytearray(2048)
    header[:8] = b'PS-X EXE'
    struct.pack_into('<I', header, 0x10, 0x80010000)
    struct.pack_into('<II', header, 0x18, 0x80010000, len(payload))
    struct.pack_into('<I', header, 0x30, 0x801FFF00)
    return header + payload


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--frontend', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--source', type=Path, default=Path(__file__).resolve().parents[1] / 'crates/quake-core/src/liquid.rs')
    parser.add_argument('--assembler', default=os.environ.get('MIPS_AS', 'mipsel-none-elf-as'))
    parser.add_argument('--objcopy', default=os.environ.get('MIPS_OBJCOPY', 'mipsel-none-elf-objcopy'))
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    text = args.source.read_text().split('unsafe fn warp_tile_64_mips', 1)[1]
    block = text.split('core::arch::asm!(', 1)[1].split('in("$4")', 1)[0]
    lines = re.findall(r'^\s*"([^"\n]*)",', block, re.MULTILINE)
    assert lines and '.set noreorder' in lines
    assembly = args.out / 'liquid.S'
    assembly.write_text('.text\n.globl liquid\nliquid:\n' + '\n'.join(lines) + '\n.set noreorder\njr $31\nnop\n')
    obj = args.out / 'liquid.o'
    raw = args.out / 'liquid.bin'
    subprocess.run([args.assembler, '-EL', '-mips1', '-o', str(obj), str(assembly)], check=True)
    subprocess.run([args.objcopy, '-O', 'binary', '-j', '.text', str(obj), str(raw)], check=True)
    routine = raw.read_bytes()
    results = []
    for case in range(3):
        source = bytes([37] * 4096) if case == 0 else bytes(((x * 3 + y * 11) ^ (x >> 2) ^ case * 19) & 255 for y in range(64) for x in range(64))
        offsets = bytes((x * 7 + case * 3) & 15 for x in range(64))
        expected = bytes(source[((y + offsets[x]) & 63) * 64 + ((x + offsets[y]) & 63)] for y in range(64) for x in range(64))
        exe = args.out / f'case-{case}.exe'
        ram_path = args.out / f'case-{case}-ram.bin'
        exe.write_bytes(harness(routine, source, offsets))
        command = [str(args.frontend), 'launch', '--path', str(exe), '--steps', '250000', '--dump-ram', str(ram_path)]
        with (args.out / f'case-{case}.log').open('w') as log:
            subprocess.run(command, stdout=log, stderr=subprocess.STDOUT, check=True)
        ram = ram_path.read_bytes()
        assert struct.unpack_from('<I', ram, 0x103000)[0] == 0x1234, 'MIPS routine did not return'
        actual = ram[0x102000:0x103000]
        mismatch = sum(a != b for a, b in zip(actual, expected))
        (args.out / f'case-{case}-actual.bin').write_bytes(actual)
        (args.out / f'case-{case}-expected.bin').write_bytes(expected)
        results.append({'case': case, 'mismatching_texels': mismatch, 'texels': 4096, 'command': command})
    result = {'source': str(args.source), 'source_sha256': hashlib.sha256(args.source.read_bytes()).hexdigest(), 'routine_sha256': hashlib.sha256(routine).hexdigest(), 'frontend': str(args.frontend.resolve()), 'frontend_sha256': hashlib.sha256(args.frontend.read_bytes()).hexdigest(), 'cases': results}
    (args.out / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    assert all(case['mismatching_texels'] == 0 for case in results), result
    print('MIPS liquid warp: all 12,288 texels match the scalar reference')


if __name__ == '__main__':
    main()
