# PS4 port: turns a frame trace's texture dump (trace_NNN.raw, rows `row` bytes apart; RAC_GNM_TRACE, RAC_GNM_DUMP_UPLOADS) into
# two pictures: NAME.rgb (the colour) and NAME.a (the alpha as grey), each as .bmp and .png, and prints how much is non-zero.
#   python ps4/tools/raw2bmp.py trace_000.raw WIDTH ROW_BYTES [TEXEL_BYTES]   (4 = RGBA8, the default; 1 = R8, e.g. a glyph atlas)
import struct, sys, zlib


def save(stem, w, h, rgb):
	"""`rgb`: w * h pixels as R, G, B bytes."""
	# 32-bit BMP, rows top first (negative height), BGRA.
	bgra = bytearray()
	for i in range(0, len(rgb), 3):
		bgra += bytes((rgb[i + 2], rgb[i + 1], rgb[i], 255))
	head = b"BM" + struct.pack("<IHHI", 54 + len(bgra), 0, 0, 54)
	info = struct.pack("<IiiHHIIiiII", 40, w, -h, 1, 32, 0, len(bgra), 0, 0, 0, 0)
	open(stem + ".bmp", "wb").write(head + info + bytes(bgra))
	# PNG: 8-bit RGB, filter 0 on every row.
	def chunk(t, d):
		return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
	rows = b"".join(bytes([0]) + bytes(rgb[y * w * 3:(y + 1) * w * 3]) for y in range(h))
	sig = bytes([0x89]) + b"PNG" + bytes([13, 10, 26, 10])
	png = sig + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows, 6)) + chunk(b"IEND", b"")
	open(stem + ".png", "wb").write(png)


raw, w, row = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
tb = int(sys.argv[4]) if len(sys.argv) > 4 else 4
b = open(raw, "rb").read()
h = len(b) // row
rgb, a = bytearray(), bytearray()
nz = [0, 0, 0, 0]
for y in range(h):
	r = b[y * row:y * row + w * tb]
	for x in range(0, len(r), tb):
		R, G, B, A = (r[x], r[x + 1], r[x + 2], r[x + 3]) if tb == 4 else (r[x], r[x], r[x], 255)
		rgb += bytes((R, G, B))
		a += bytes((A, A, A))
		for i, v in enumerate((R, G, B, A)):
			nz[i] += v != 0
stem = raw.rsplit(".", 1)[0]
save(stem + ".rgb", w, h, rgb)
save(stem + ".a", w, h, a)
print(f"{raw}: {w}x{h}, texels with non-zero R {nz[0]} G {nz[1]} B {nz[2]} A {nz[3]} of {w * h}")
