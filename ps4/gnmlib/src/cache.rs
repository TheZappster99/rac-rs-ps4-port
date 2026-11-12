//! The PS4 shader cache file (PS4_GPU_DESIGN.md, "The shader cache key and the capture"): every pipeline the renderer
//! creates, compiled on the PC ahead of time (the console has no shader compiler), looked up by the key
//! `wgpu-hal/src/ps4key.rs` computes. `ps4/tools/shcache` writes it from the capture; the PS4 back end reads it.
//!
//! Layout (little-endian):
//!   header  "RPSC", u32 version (1), u32 entry count, u32 reserved
//!   index   count x { u64 key, u32 offset, u32 length }, sorted by key (offsets from the file start)
//!   entries each: u32 length + the capture record text (`<key>.rec`, format in wgpu-hal's vulkan/ps4cap.rs), padded to 4;
//!           u32 length + radv's set memory layout text (`<key>.sets`, format in ps4/tools/vkc/vkrec.c), padded to 4;
//!           u32 stage count, then per stage u32 length + a .rsh entry (`shader::Shader`), in the record's stage order.

const MAGIC: &[u8; 4] = b"RPSC";
const VERSION: u32 = 1;
const HEADER: usize = 16;
const INDEX_ENTRY: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub enum CacheError {
	TooShort,
	BadMagic,
	BadVersion(u32),
	/// The index or an entry points outside the file, or the index is not sorted.
	Corrupt(&'static str),
}

/// One pipeline of the cache. Everything borrows the file's bytes.
#[derive(Debug)]
pub struct Entry<'a> {
	pub key: u64,
	/// The capture record: set layouts, stages, vertex input and fixed state.
	pub rec: &'a str,
	/// radv's descriptor set memory layout (binding offsets) for the record's sets.
	pub sets: &'a str,
	/// The compiled stages (.rsh), in the record's stage order.
	pub stages: Vec<&'a [u8]>,
}

/// A parsed cache file.
pub struct Cache<'a> {
	data: &'a [u8],
	count: usize,
}

fn u32_at(b: &[u8], o: usize) -> Option<u32> {
	Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}

fn u64_at(b: &[u8], o: usize) -> Option<u64> {
	Some(u64::from_le_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// A length-prefixed block at `o`; returns it and the offset after it (padded to 4).
fn block(b: &[u8], o: usize) -> Option<(&[u8], usize)> {
	let n = u32_at(b, o)? as usize;
	let s = b.get(o + 4..o + 4 + n)?;
	Some((s, (o + 4 + n).next_multiple_of(4)))
}

impl<'a> Cache<'a> {
	pub fn parse(data: &'a [u8]) -> Result<Cache<'a>, CacheError> {
		if data.len() < HEADER {
			return Err(CacheError::TooShort);
		}
		if &data[0..4] != MAGIC {
			return Err(CacheError::BadMagic);
		}
		let v = u32_at(data, 4).unwrap();
		if v != VERSION {
			return Err(CacheError::BadVersion(v));
		}
		let count = u32_at(data, 8).unwrap() as usize;
		if HEADER + count * INDEX_ENTRY > data.len() {
			return Err(CacheError::Corrupt("index past the end"));
		}
		let c = Cache { data, count };
		let mut last = None;
		for i in 0..count {
			let (k, off, len) = c.index(i);
			if last.is_some_and(|l| l >= k) {
				return Err(CacheError::Corrupt("index not sorted"));
			}
			last = Some(k);
			if off < HEADER + count * INDEX_ENTRY || off + len > data.len() {
				return Err(CacheError::Corrupt("entry outside the file"));
			}
		}
		Ok(c)
	}

	fn index(&self, i: usize) -> (u64, usize, usize) {
		let o = HEADER + i * INDEX_ENTRY;
		(u64_at(self.data, o).unwrap(), u32_at(self.data, o + 8).unwrap() as usize, u32_at(self.data, o + 12).unwrap() as usize)
	}

	pub fn len(&self) -> usize {
		self.count
	}

	pub fn is_empty(&self) -> bool {
		self.count == 0
	}

	/// The keys, in ascending order.
	pub fn keys(&self) -> impl Iterator<Item = u64> + '_ {
		(0..self.count).map(|i| self.index(i).0)
	}

	/// The pipeline with `key`, or `None` (not captured on the PC). `Err` = the entry is damaged.
	pub fn get(&self, key: u64) -> Result<Option<Entry<'a>>, CacheError> {
		let (mut lo, mut hi) = (0, self.count);
		while lo < hi {
			let mid = (lo + hi) / 2;
			let (k, off, len) = self.index(mid);
			match k.cmp(&key) {
				core::cmp::Ordering::Less => lo = mid + 1,
				core::cmp::Ordering::Greater => hi = mid,
				core::cmp::Ordering::Equal => return Self::entry(key, &self.data[off..off + len]).map(Some),
			}
		}
		Ok(None)
	}

	fn entry(key: u64, e: &'a [u8]) -> Result<Entry<'a>, CacheError> {
		let bad = CacheError::Corrupt("entry");
		let (rec, o) = block(e, 0).ok_or(CacheError::Corrupt("entry record"))?;
		let (sets, o) = block(e, o).ok_or(CacheError::Corrupt("entry sets"))?;
		let n = u32_at(e, o).ok_or(CacheError::Corrupt("entry stage count"))? as usize;
		let mut o = o + 4;
		let mut stages = Vec::with_capacity(n);
		for _ in 0..n {
			let (s, next) = block(e, o).ok_or(CacheError::Corrupt("entry stage"))?;
			stages.push(s);
			o = next;
		}
		let rec = core::str::from_utf8(rec).map_err(|_| CacheError::Corrupt("record text"))?;
		let sets = core::str::from_utf8(sets).map_err(|_| bad)?;
		Ok(Entry { key, rec, sets, stages })
	}
}

/// One pipeline to write.
pub struct NewEntry<'a> {
	pub key: u64,
	pub rec: &'a str,
	pub sets: &'a str,
	pub stages: Vec<&'a [u8]>,
}

fn put_block(out: &mut Vec<u8>, b: &[u8]) {
	out.extend_from_slice(&(b.len() as u32).to_le_bytes());
	out.extend_from_slice(b);
	while out.len() % 4 != 0 {
		out.push(0);
	}
}

/// Writes a cache file. Keys must be unique.
pub fn write(entries: &mut [NewEntry<'_>]) -> Vec<u8> {
	entries.sort_by_key(|e| e.key);
	let mut out = Vec::new();
	out.extend_from_slice(MAGIC);
	out.extend_from_slice(&VERSION.to_le_bytes());
	out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
	out.extend_from_slice(&0u32.to_le_bytes());
	let index_at = out.len();
	out.resize(index_at + entries.len() * INDEX_ENTRY, 0);
	for (i, e) in entries.iter().enumerate() {
		let start = out.len();
		put_block(&mut out, e.rec.as_bytes());
		put_block(&mut out, e.sets.as_bytes());
		out.extend_from_slice(&(e.stages.len() as u32).to_le_bytes());
		for s in &e.stages {
			put_block(&mut out, s);
		}
		let len = out.len() - start;
		let o = index_at + i * INDEX_ENTRY;
		out[o..o + 8].copy_from_slice(&e.key.to_le_bytes());
		out[o + 8..o + 12].copy_from_slice(&(start as u32).to_le_bytes());
		out[o + 12..o + 16].copy_from_slice(&(len as u32).to_le_bytes());
	}
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn round_trip() {
		let a = [1u8, 2, 3, 4];
		let b = [5u8; 8];
		let mut es = vec![
			NewEntry { key: 9, rec: "rec 1\nkey 9\n", sets: "set 0 16\n", stages: vec![&a, &b] },
			NewEntry { key: 3, rec: "rec 1\nkey 3\n", sets: "", stages: vec![&b] },
		];
		let file = write(&mut es);
		let c = Cache::parse(&file).unwrap();
		assert_eq!(c.len(), 2);
		assert_eq!(c.keys().collect::<Vec<_>>(), vec![3, 9]);
		let e = c.get(9).unwrap().unwrap();
		assert_eq!(e.rec, "rec 1\nkey 9\n");
		assert_eq!(e.sets, "set 0 16\n");
		assert_eq!(e.stages, vec![&a[..], &b[..]]);
		assert!(c.get(4).unwrap().is_none());
		assert_eq!(c.get(3).unwrap().unwrap().stages, vec![&b[..]]);
		let mut bad = file.clone();
		bad[0] = b'X';
		assert_eq!(Cache::parse(&bad).err(), Some(CacheError::BadMagic));
	}
}
