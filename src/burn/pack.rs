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

/// The most a journal's compressed copy may decompress to: one
/// gibibyte, the ceiling the tin gives an object that does not say how
/// large it is. A compressed journal can come from another machine (a
/// run carried in a parcel), and a few kilobytes of zstd can stand for
/// gigabytes, so it is never decompressed without a bound. A journal
/// larger than this is left plain when its run ends, so cig writes no
/// compressed journal that it would refuse to read back.
pub const MAX_JOURNAL: u64 = 1 << 30;

/// `CIG_JOURNAL_ZST=on` compresses a finished run's journal to
/// `journal.jsonl.zst`; the default keeps it plain. Compression is opt-in
/// so the on-disk layout stays the one `docs/BURN.md` documents, and so
/// any tool that reads a run's folder by name finds the file it expects.
/// The reader side is symmetric: `read_journal_text` returns either form,
/// and `ensure_plain` makes the plain file again for a command that
/// appends to it. A compressed journal written by an earlier cig is
/// read, verified and unburned unchanged.
pub fn zst_wanted() -> bool {
    matches!(
        std::env::var("CIG_JOURNAL_ZST").as_deref(),
        Ok("on") | Ok("1") | Ok("yes")
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
    read_journal_text_within(run_dir, MAX_JOURNAL)
}

/// [`read_journal_text`] with the ceiling given: a compressed journal
/// that comes to more than `max` bytes is refused, and is never
/// decompressed past them.
fn read_journal_text_within(run_dir: &Path, max: u64) -> io::Result<String> {
    match fs::read_to_string(run_dir.join(JOURNAL)) {
        Ok(t) => Ok(t),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let zst = run_dir.join(JOURNAL_ZST);
            let f = match fs::File::open(&zst) {
                Ok(f) => f,
                Err(z) if z.kind() == io::ErrorKind::NotFound => return Err(e),
                Err(z) => return Err(z),
            };
            let mut bytes = Vec::new();
            decompressing(f, max)?.read_to_end(&mut bytes)?;
            if bytes.len() as u64 > max {
                return Err(past_the_ceiling(&zst, max));
            }
            String::from_utf8(bytes).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
        }
        Err(e) => Err(e),
    }
}

/// A compressed journal, decompressing as it is read: at most one byte
/// more than `max`, so whoever reads it to its end can tell a journal
/// past the ceiling from one at it without ever holding more. Whatever
/// wrote the frame, and whether or not it says how large it is.
fn decompressing(zst: fs::File, max: u64) -> io::Result<impl Read> {
    Ok(zstd::stream::read::Decoder::new(zst)?.take(max.saturating_add(1)))
}

fn past_the_ceiling(zst: &Path, max: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{} decompresses past the {max} bytes a journal may be",
            zst.display()
        ),
    )
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
    ensure_plain_within(run_dir, MAX_JOURNAL)
}

/// [`ensure_plain`] with the ceiling given. The text goes from the
/// compressed copy to the plain file as it is decompressed, so it is
/// never held whole; one that comes to more than `max` bytes is
/// refused, and the compressed copy stays the journal.
fn ensure_plain_within(run_dir: &Path, max: u64) -> io::Result<()> {
    let plain = run_dir.join(JOURNAL);
    let zst = run_dir.join(JOURNAL_ZST);
    if plain.exists() || !zst.exists() {
        return Ok(());
    }
    let mut text = decompressing(fs::File::open(&zst)?, max)?;
    let tmp = run_dir.join("journal.jsonl.plain");
    let written = (|| -> io::Result<()> {
        let mut f = fs::File::create(&tmp)?;
        if io::copy(&mut text, &mut f)? > max {
            return Err(past_the_ceiling(&zst, max));
        }
        super::journal::full_sync_data(&f)
    })();
    if let Err(e) = written {
        // What was written so far is no journal: nothing is left of it.
        let _ = fs::remove_file(&tmp);
        return Err(e);
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
    compress_finished_within(run_dir, MAX_JOURNAL)
}

/// [`compress_finished`] with the ceiling given: a journal of more than
/// `max` bytes stays plain, which is always readable.
fn compress_finished_within(run_dir: &Path, max: u64) -> io::Result<bool> {
    let plain = run_dir.join(JOURNAL);
    let mut bytes = Vec::new();
    match fs::File::open(&plain) {
        Ok(mut f) => {
            if f.metadata()?.len() > max {
                return Ok(false);
            }
            f.read_to_end(&mut bytes)?
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    if bytes.is_empty() || bytes.len() as u64 > max {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A run's folder holding a plain journal of `lines` lines.
    fn run_with_journal(lines: usize) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("a scratch folder");
        let text: String = (0..lines)
            .map(|n| {
                format!("{{\"seq\":{n},\"op\":\"write\",\"path\":\"/srv/app/out-{n}.txt\"}}\n")
            })
            .collect();
        fs::write(dir.path().join(JOURNAL), &text).expect("written");
        (dir, text)
    }

    #[test]
    fn a_compressed_journal_reads_back_as_the_text_it_was() {
        let (dir, text) = run_with_journal(200);
        assert!(compress_finished(dir.path()).expect("compressed"));
        assert!(!dir.path().join(JOURNAL).exists());
        assert!(zst_path(dir.path()).exists());
        assert_eq!(read_journal_text(dir.path()).expect("read"), text);
        // And at exactly the ceiling: the bound refuses what is past
        // it, not what reaches it.
        let at = text.len() as u64;
        assert_eq!(
            read_journal_text_within(dir.path(), at).expect("read"),
            text
        );
        // Made plain again, it is the same text, and the compressed
        // copy is gone.
        ensure_plain(dir.path()).expect("plain again");
        assert_eq!(
            fs::read_to_string(dir.path().join(JOURNAL)).expect("read"),
            text
        );
        assert!(!zst_path(dir.path()).exists());
    }

    #[test]
    fn a_compressed_journal_past_the_ceiling_is_refused_and_not_decompressed_past_it() {
        let (dir, text) = run_with_journal(200);
        assert!(compress_finished(dir.path()).expect("compressed"));
        let max = text.len() as u64 - 1;
        let e = read_journal_text_within(dir.path(), max).expect_err("past the ceiling");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().contains("decompresses past"), "{e}");
        // The reader gives one byte past the ceiling and no more, however
        // much the frame holds: a megabyte of one letter in a few bytes.
        let bomb = dir.path().join("bomb.zst");
        fs::write(
            &bomb,
            zstd::bulk::compress(&vec![b'a'; 1 << 20], LEVEL).expect("compressed"),
        )
        .expect("written");
        assert!(fs::metadata(&bomb).expect("there").len() < 1024);
        let mut got = Vec::new();
        decompressing(fs::File::open(&bomb).expect("opens"), 4096)
            .expect("a frame")
            .read_to_end(&mut got)
            .expect("read");
        assert_eq!(got.len(), 4097);
    }

    #[test]
    fn making_a_journal_plain_past_the_ceiling_leaves_the_compressed_copy_and_nothing_else() {
        let (dir, text) = run_with_journal(200);
        assert!(compress_finished(dir.path()).expect("compressed"));
        let e = ensure_plain_within(dir.path(), text.len() as u64 - 1).expect_err("past it");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        let left: Vec<String> = fs::read_dir(dir.path())
            .expect("lists")
            .map(|f| {
                f.expect("an entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            left,
            vec![JOURNAL_ZST.to_string()],
            "no half-written journal"
        );
        // The compressed copy is still the journal.
        assert_eq!(read_journal_text(dir.path()).expect("read"), text);
    }

    #[test]
    fn a_journal_larger_than_the_ceiling_stays_plain() {
        let (dir, text) = run_with_journal(200);
        assert!(!compress_finished_within(dir.path(), text.len() as u64 - 1).expect("looked"));
        assert!(dir.path().join(JOURNAL).exists());
        assert!(!zst_path(dir.path()).exists());
        // At the ceiling it is compressed: what is written is what is
        // read back under the same bound.
        assert!(compress_finished_within(dir.path(), text.len() as u64).expect("compressed"));
        assert_eq!(
            read_journal_text_within(dir.path(), text.len() as u64).expect("read"),
            text
        );
    }

    #[test]
    fn a_frame_that_does_not_say_how_large_it_is_is_read_under_the_same_bound() {
        // A streaming encoder writes no size in the frame's header.
        let (dir, text) = run_with_journal(50);
        let packed = zstd::stream::encode_all(text.as_bytes(), LEVEL).expect("compressed");
        assert_eq!(
            zstd::zstd_safe::get_frame_content_size(&packed)
                .ok()
                .flatten(),
            None,
            "no size in the header"
        );
        fs::write(zst_path(dir.path()), packed).expect("written");
        fs::remove_file(dir.path().join(JOURNAL)).expect("removed");
        assert_eq!(read_journal_text(dir.path()).expect("read"), text);
        assert!(read_journal_text_within(dir.path(), 100).is_err());
    }
}
