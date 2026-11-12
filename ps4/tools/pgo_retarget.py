"""PS4 port: retargets the PC release's PGO profile to the PS4 build.

The profile (generated/pgo/<serial>-<tag>.profdata, made by setup.ps1 from the release's pgo/<serial>.profdata.gz) names functions by
their mangled symbol, and the mangled name of every function holds its crate's disambiguator (a hash). ps4/app is its own cargo
workspace and its own target, so every crate has other hashes there, and the profile would match nothing. The crate names are the
same, so this maps each crate's hash from the PC exe to the PS4 ELF (built once without the profile) and rewrites the names.

  pgo_retarget.py <llvm-nm> <llvm-profdata> <pc exe> <ps4 elf> <pc.profdata> <out.profdata>

The output is derived from the disc-derived profile: it stays in build-ps4/ and is never committed or shared.
"""
import re, subprocess, sys, os, tempfile

nm, pd, pc_exe, ps4_elf, src, dst = sys.argv[1:7]


def crates(exe):
	out = subprocess.run([nm, '--defined-only', exe], capture_output=True, text=True, errors='replace').stdout
	m = {}
	for mm in re.finditer(r'C[sc]([A-Za-z0-9]{8,16})_(\d{1,2})([a-z_][a-z0-9_]*)', out):
		h, n, rest = mm.group(1), int(mm.group(2)), mm.group(3)
		name = rest[:n]
		if len(name) == n and name.startswith(('ee_', 'rec_', 'rac_')):
			m.setdefault(name, set()).add(h)
	return m


a, b = crates(pc_exe), crates(ps4_elf)
rep = {}
for name in a:
	if name in b and len(a[name]) == 1 and len(b[name]) == 1:
		rep[f"{next(iter(a[name]))}_{len(name)}{name}"] = f"{next(iter(b[name]))}_{len(name)}{name}"
if not any('ee_oracle' in k for k in rep):
	sys.exit('pgo_retarget: ee_oracle not found in both binaries')
pat = re.compile('|'.join(re.escape(k) for k in sorted(rep, key=len, reverse=True)))
tmp = tempfile.mkdtemp()
txt = os.path.join(tmp, 'p.txt')
subprocess.run([pd, 'merge', '--text', '-o', txt, src], check=True)
n = 0
with open(txt, encoding='utf8') as f, open(txt + '.new', 'w', encoding='utf8', newline='\n') as o:
	for line in f:
		if line.startswith('_R'):
			line, c = pat.subn(lambda mo: rep[mo.group(0)], line)
			n += c
		o.write(line)
subprocess.run([pd, 'merge', '-o', dst, txt + '.new'], check=True)
print(f"pgo_retarget: {len(rep)} crates mapped, {n} names rewritten -> {dst}")
