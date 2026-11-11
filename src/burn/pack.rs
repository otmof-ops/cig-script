// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! zstd at rest (#147): a finished run's journal is kept as
//! `journal.jsonl.zst`, and the small before-states of a run are packed into
//! one file under its `snapshots/` folder instead of one file each.
//!
//! The journal's lines are never changed by any of this. The chain (each
//! line's `prev`) is checked over the decompressed lines exactly as it is
//! over a plain file, so `cig runs --verify` judges a compressed journal the
//! way it judges any other, and a line edited inside the stream breaks it.
//! A journal that must be appended to (an `unburn` writes marker lines) is
//! made plain again first, and compressed again when that command ends.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// zstd's level 3: the library's default, quick and good on text.
pub const LEVEL: i32 = 3;

pub const JOURNAL: &str = "journal.jsonl";
pub const JOURNAL_ZST: &str = "journal.jsonl.zst";

/// `CIG_JOURNAL_ZST=off` keeps finished journals plain.
pub fn zst_wanted() -> bool {
    !matches!(
        std::env::var("CIG_JOURNAL_ZST").as_deref(),
        Ok("off") | Ok("0") | Ok("no")
    )
}

/// Compress this run's journal now that the command is done with it. A
/// failure leaves the plain journal, which is always readable.
pub fn settle(run_dir: &Path) {
    if zst_wanted() {
        let _ = compress_finished(run_dir);
    }
}

/// Is there a journal for this run, plain or compressed?
pub fn journal_exists(run_dir: &Path) -> bool {
    run_dir.join(JOURNAL).exists() || run_dir.join(JOURNAL_ZST).exists()
}

/// The journal's text as the chain sees it, whichever form is on disk. A
/// plain file wins when both exist: it is the newer one (a crash between
/// writing the compressed copy and removing the plain one leaves both).
pub fn read_journal_text(run_dir: &Path) -> io::Result<String> {
    match fs::read_to_string(run_dir.join(JOURNAL)) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let f = match fs::File::open(run_dir.join(JOURNAL_ZST)) {
                Ok(f) => f,
                Err(z) if z.kind() == io::ErrorKind::NotFound => return Err(e),
                Err(z) => return Err(z),
            };
            let bytes = zstd::stream::decode_all(io::BufReader::new(f))?;
            String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Err(e) => Err(e),
    }
}

/// The SHA-256 of the journal's text, whichever form it is in: two copies
/// of one run compare equal when their lines do.
pub fn journal_digest(run_dir: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let text = read_journal_text(run_dir).ok()?;
    Some(hex::encode(Sha256::digest(text.as_bytes())))
}

/// Make the journal plain again, for a command that appends to it or
/// rewrites it. Does nothing when it already is.
pub fn ensure_plain(run_dir: &Path) -> io::Result<()> {
    let plain = run_dir.join(JOURNAL);
    let zst = run_dir.join(JOURNAL_ZST);
    if plain.exists() || !zst.exists() {
        return Ok(());
    }
    let bytes = zstd::stream::decode_all(io::BufReader::new(fs::File::open(&zst)?))?;
    let tmp = run_dir.join("journal.jsonl.plain");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        super::journal::full_sync_data(&f)?;
    }
    fs::rename(&tmp, &plain)?;
    super::journal::sync_dir_pub(run_dir)?;
    let _ = fs::remove_file(&zst);
    let _ = super::journal::sync_dir_pub(run_dir);
    Ok(())
}

/// Compress a finished run's journal. The compressed copy is written,
/// flushed and named before the plain one is removed, so a crash leaves
/// one readable form or both, never neither. Returns whether it compressed.
pub fn compress_finished(run_dir: &Path) -> io::Result<bool> {
    let plain = run_dir.join(JOURNAL);
    let mut bytes = Vec::new();
    match fs::File::open(&plain) {
        Ok(mut f) => f.read_to_end(&mut bytes)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if bytes.is_empty() {
        return Ok(false);
    }
    let packed = zstd::bulk::compress(&bytes, LEVEL)?;
    let tmp = run_dir.join("journal.jsonl.zst.tmp");
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&packed)?;
        super::journal::full_sync_data(&f)?;
    }
    fs::rename(&tmp, run_dir.join(JOURNAL_ZST))?;
    super::journal::sync_dir_pub(run_dir)?;
    fs::remove_file(&plain)?;
    let _ = super::journal::sync_dir_pub(run_dir);
    Ok(true)
}

/// Where the compressed journal would be.
pub fn zst_path(run_dir: &Path) -> PathBuf {
    run_dir.join(JOURNAL_ZST)
}
