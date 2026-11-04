#!/usr/bin/env python3
"""Append the native runtime library to a `coil` binary.

    scripts/embed_runtime.py COIL LIBCOIL_RT

`coil package` then links AOT executables without a `libcoil_rt.a` beside
it: it writes the embedded copy to its cache on first use
(`package_app::embedded_runtime`). Layout after the binary: the library,
its length (u64 little-endian), then the 8-byte magic `COILRT\\x00\\x01`.
Embedding twice is refused.
"""
import struct
import sys

MAGIC = b"COILRT\x00\x01"


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__.strip().splitlines()[2].strip(), file=sys.stderr)
        return 2
    coil, lib = sys.argv[1], sys.argv[2]
    with open(coil, "rb") as f:
        f.seek(0, 2)
        if f.tell() >= 16:
            f.seek(-8, 2)
            if f.read(8) == MAGIC:
                print(f"error: {coil} already embeds a runtime", file=sys.stderr)
                return 1
    with open(lib, "rb") as f:
        data = f.read()
    with open(coil, "ab") as f:
        f.write(data)
        f.write(struct.pack("<Q", len(data)))
        f.write(MAGIC)
    print(f"embedded {lib} ({len(data)} bytes) in {coil}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
