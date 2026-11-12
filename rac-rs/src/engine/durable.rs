//! PS4 port: the file replace every save and settings file uses (temp file + rename), forced to disk on the console. Lives in
//! rac-engine so the engine's own files (pcopts, mods settings, texture pack store) use it too; rac1 retail/memcard.rs re-exports it.

/// Writes `bytes` to `tmp`, then puts `tmp` in place of `path` (the temp file + rename every save and settings file uses, so a
/// half-written file never replaces a good one). On the PS4 the console caches file writes and the player may close the app with
/// the PS button, which runs no exit code: there the new file and the folder entry are forced to disk (fsync) before this
/// returns, and a rename that cannot replace an existing file falls back to removing it first (`restore` covers the gap). The PC
/// does the plain write and rename, as before.
pub fn write_replace(tmp: &std::path::Path, path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
	#[cfg(target_os = "freebsd")]
	{
		write_synced(tmp, bytes)?;
		if let Err(e) = std::fs::rename(tmp, path) {
			eprintln!("ps4: rename onto {} failed ({e}), replacing it", path.display());
			let _ = std::fs::remove_file(path);
			std::fs::rename(tmp, path)?;
		}
		sync_dir(path);
		Ok(())
	}
	#[cfg(not(target_os = "freebsd"))]
	{
		std::fs::write(tmp, bytes)?;
		std::fs::rename(tmp, path)
	}
}

/// PS4: `bytes` into `path`, forced to disk before this returns.
#[cfg(target_os = "freebsd")]
pub fn write_synced(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
	use std::io::Write;
	let mut f = std::fs::File::create(path)?;
	f.write_all(bytes)?;
	f.sync_all()
}

/// PS4: forces the folder that holds `path` to disk, so a rename into it survives the app being closed. A failure is logged once
/// (the file data itself is already on disk).
#[cfg(target_os = "freebsd")]
fn sync_dir(path: &std::path::Path) {
	static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
	let Some(dir) = path.parent() else { return };
	if let Err(e) = std::fs::File::open(dir).and_then(|d| d.sync_all()) {
		if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
			eprintln!("ps4: cannot sync the folder {}: {e}", dir.display());
		}
	}
}
