"""Install a PKG on the PS4 from this PC, through flatz's Remote Package Installer (RPI) homebrew app.

RPI listens on port 12800 while it is open on the console. It does not take the file itself: it is given an HTTP
URL and the console downloads the PKG from there (with Range requests, which python's http.server does not answer,
hence the small server below). So this script serves the one PKG over HTTP, asks RPI to install it, and follows the
task until the console has it.

  python -I ps4/tools/rpi.py install build-ps4/to-console/IV0000-RACR00009_00-RACRSAPP00000003.pkg --replace
  python -I ps4/tools/rpi.py exists RACR00009
  python -I ps4/tools/rpi.py uninstall RACR00009

--replace uninstalls the installed copy first: our PKGs keep version 01.00, and the console refuses to install the same
version over itself. Windows Firewall must allow inbound TCP on --port (default 12801) for the console to fetch the file.
"""

import argparse
import http.server
import json
import os
import socket
import sys
import threading
import time
import urllib.request

PS4 = "10.10.0.6"
RPI_PORT = 12800


def api(ps4, name, body):
	req = urllib.request.Request(
		f"http://{ps4}:{RPI_PORT}/api/{name}", data=json.dumps(body).encode(), method="POST",
		headers={"Content-Type": "application/json"})
	with urllib.request.urlopen(req, timeout=30) as r:
		text = r.read().decode(errors="replace")
	# RPI's JSON sometimes carries a trailing NUL or newline.
	return json.loads(text.strip("\x00\r\n "))


def title_id(pkg):
	# The content id ("IV0000-RACR00009_00-RACRSAPP00000003") sits at 0x40 in the PKG header; the title id is its middle.
	with open(pkg, "rb") as f:
		f.seek(0x40)
		cid = f.read(36).decode("ascii", "replace")
	return cid[7:16]


def local_ip_towards(ps4):
	# The address of the interface that routes to the console (the PC has VPN / WSL adapters too).
	s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
	try:
		s.connect((ps4, RPI_PORT))
		return s.getsockname()[0]
	finally:
		s.close()


def make_handler(path):
	name = os.path.basename(path)
	size = os.path.getsize(path)

	class H(http.server.BaseHTTPRequestHandler):
		protocol_version = "HTTP/1.1"

		def log_message(self, fmt, *args):
			pass

		def _range(self):
			r = self.headers.get("Range")
			if not r or not r.startswith("bytes="):
				return None
			a, _, b = r[6:].split(",")[0].partition("-")
			if a == "":
				start, end = max(0, size - int(b)), size - 1
			else:
				start, end = int(a), (int(b) if b else size - 1)
			return start, min(end, size - 1)

		def _head(self):
			if self.path.lstrip("/").split("?")[0] != name:
				self.send_error(404)
				return None
			rng = self._range()
			if rng:
				start, end = rng
				self.send_response(206)
				self.send_header("Content-Range", f"bytes {start}-{end}/{size}")
			else:
				start, end = 0, size - 1
				self.send_response(200)
			self.send_header("Content-Type", "application/octet-stream")
			self.send_header("Accept-Ranges", "bytes")
			self.send_header("Content-Length", str(end - start + 1))
			self.end_headers()
			return start, end

		def do_HEAD(self):
			self._head()

		def do_GET(self):
			span = self._head()
			if not span:
				return
			start, end = span
			left = end - start + 1
			with open(path, "rb") as f:
				f.seek(start)
				try:
					while left > 0:
						chunk = f.read(min(left, 1 << 20))
						if not chunk:
							break
						self.wfile.write(chunk)
						left -= len(chunk)
				except (BrokenPipeError, ConnectionResetError):
					pass

	return H


def wait_gone(ps4, tid, secs=60):
	for _ in range(secs):
		if api(ps4, "is_exists", {"title_id": tid}).get("exists") != "true":
			return True
		time.sleep(1)
	return False


def cmd_install(a):
	pkg = os.path.abspath(a.pkg)
	tid = title_id(pkg)
	print(f"{os.path.basename(pkg)}: title {tid}, {os.path.getsize(pkg) / 1e6:.1f} MB")
	if a.replace and api(a.ps4, "is_exists", {"title_id": tid}).get("exists") == "true":
		print(f"uninstalling the installed {tid} first")
		print(" ", api(a.ps4, "uninstall_game", {"title_id": tid}))
		if not wait_gone(a.ps4, tid):
			sys.exit("the old copy is still there after 60 s")

	host = local_ip_towards(a.ps4)
	srv = http.server.ThreadingHTTPServer((host, a.port), make_handler(pkg))
	threading.Thread(target=srv.serve_forever, daemon=True).start()
	url = f"http://{host}:{a.port}/{os.path.basename(pkg)}"
	print("serving", url)
	try:
		r = api(a.ps4, "install", {"type": "direct", "packages": [url]})
		print(" ", r)
		if r.get("status") != "success":
			sys.exit("RPI refused the install")
		task = r["task_id"]
		last = None
		while True:
			time.sleep(2)
			p = api(a.ps4, "get_task_progress", {"task_id": task})
			if p.get("status") != "success":
				# RPI drops a finished task, so a failed lookup after progress means it is done.
				break
			if p.get("error"):
				sys.exit(f"install failed on the console: {p}")
			done, total = p.get("transferred_total", 0), p.get("length_total", 0)
			line = f"  {done / 1e6:.1f} / {total / 1e6:.1f} MB"
			if line != last:
				print(line)
				last = line
			if total and done >= total and p.get("rest_sec_total", 0) == 0:
				break
		# Let the console finish unpacking before the server goes away.
		for _ in range(120):
			if api(a.ps4, "is_exists", {"title_id": tid}).get("exists") == "true":
				print(f"installed: {tid}")
				return
			time.sleep(1)
		sys.exit("the download finished but the app does not show as installed")
	finally:
		srv.shutdown()


def main():
	p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
	p.add_argument("--ps4", default=PS4)
	sub = p.add_subparsers(dest="cmd", required=True)
	i = sub.add_parser("install")
	i.add_argument("pkg")
	i.add_argument("--replace", action="store_true", help="uninstall the installed copy of this title first")
	i.add_argument("--port", type=int, default=12801)
	for name in ("exists", "uninstall"):
		sub.add_parser(name).add_argument("title_id")
	a = p.parse_args()
	try:
		if a.cmd == "install":
			cmd_install(a)
		elif a.cmd == "exists":
			print(api(a.ps4, "is_exists", {"title_id": a.title_id}))
		else:
			print(api(a.ps4, "uninstall_game", {"title_id": a.title_id}))
	except (ConnectionRefusedError, urllib.error.URLError) as e:
		sys.exit(f"cannot reach Remote Package Installer on {a.ps4}:{RPI_PORT} (is the app open on the console?): {e}")


if __name__ == "__main__":
	main()
