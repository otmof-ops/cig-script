// SPDX-FileCopyrightText: 2026 Jay Taylor (https://github.com/otmof-ops/cig-script)
// SPDX-License-Identifier: FSL-1.1-ALv2

//! The parcel: one run's record, every tin object and snapshot its journal
//! refers to, and its compensations, as a single file (`<run>.cigparcel`, a
//! gzipped tar), imported on another machine so `cig unburn` works there.
//! Every file in it is named in a manifest with its SHA-256; an import
//! checks every one before it writes anything, maps the pack's roots and
//! the working directory to where they are here, and records the mapping
//! in the run's record so the journal, which is never rewritten (its chain
//! stays whole), reads through it.

use super::journal::{Before, Journal};
use super::runs::RunRecord;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// The file extension a parcel carries.
pub const EXT: &str = "cigparcel";
/// The shape of a manifest this cig writes.
pub const FORMAT: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub format: u32,
    /// The cig that wrote the parcel, for the humidor.
    pub cig: String,
    pub run: String,
    pub created: String,
    pub origin: Origin,
    /// The pack's roots, each with a name to map on import.
    pub roots: Vec<Root>,
    /// Every file in the archive but this one, with its SHA-256.
    pub files: BTreeMap<String, String>,
    pub sealed: SealedNote,
    /// Blobs git held on the machine that ran, carried as plain objects.
    #[serde(default)]
    pub git_blobs: usize,
    /// When the run was itself imported before this parcel was written: the
    /// map its record carried (the paths of the machine that ran it, which
    /// the journal still names, to the paths of the machine this parcel
    /// comes from), so the next import maps both to its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prior: Vec<(PathBuf, PathBuf)>,
    /// `cig parcel --with-key`: the sealing machine's master key in hex,
    /// so the restoring machine can open the parcel's sealed objects (and
    /// re-seal them under its own key on the way in). The parcel is a
    /// secret while it carries this; the writer is told so, and the reader
    /// shown the fingerprint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealing_key: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Origin {
    pub state_dir: PathBuf,
    pub cwd: String,
    #[serde(default)]
    pub host: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Root {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SealedNote {
    pub objects: usize,
    /// A fingerprint of the key that sealed them, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// What `cig parcel` wrote.
#[derive(Debug, Serialize)]
pub struct Written {
    pub path: PathBuf,
    pub files: usize,
    pub bytes: u64,
    pub manifest: Manifest,
    /// The SHA-256 of `manifest.json` as written: what the parcel's pass
    /// vouches for (cig-script#186). The manifest names every other file
    /// with its own SHA-256, so this one digest covers the whole parcel.
    pub manifest_sha256: String,
}

/// `Written`, redacted for `--json` display: the raw sealing key is
/// stripped from the embedded manifest and replaced with its
/// fingerprint (SEC-004 M1). The manifest written INTO the parcel
/// still carries the real key — a parcel's purpose when `--with-key`
/// is asked for — but a user looking at `cig parcel --json` output
/// sees no plaintext key there.
pub fn written_for_display(w: &Written) -> serde_json::Value {
    let mut v = serde_json::to_value(w).unwrap_or(serde_json::Value::Null);
    if let Some(obj) = v.as_object_mut() {
        if let Some(mv) = obj.get_mut("manifest") {
            *mv = manifest_for_display(&w.manifest);
        }
    }
    v
}

/// A manifest redacted for display: `sealing_key` removed, a derived
/// `sealing_key_fingerprint` added when one was carried (SEC-004 M1).
pub fn manifest_for_display(m: &Manifest) -> serde_json::Value {
    let mut v = serde_json::to_value(m).unwrap_or(serde_json::Value::Null);
    let fingerprint = m
        .sealing_key
        .as_deref()
        .and_then(|hex| {
            // The hex-decoded Vec sits in this scope only, and is wiped
            // before it goes.
            let mut bytes = hex::decode(hex).ok()?;
            let key = super::seal::Key::try_from(bytes.as_slice()).ok();
            super::seal::wipe_bytes(&mut bytes);
            key
        })
        .map(|k| super::seal::fingerprint(&k));
    if let Some(obj) = v.as_object_mut() {
        obj.remove("sealing_key");
        if let Some(fp) = fingerprint {
            obj.insert(
                "sealing_key_fingerprint".to_string(),
                serde_json::Value::String(fp),
            );
        }
    }
    v
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .or_else(|| {
            fs::read_to_string("/etc/hostname")
                .ok()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default()
}

/// Names for the pack's roots: the last component, numbered when two share it.
pub fn root_names(roots: &[PathBuf]) -> Vec<Root> {
    let mut out: Vec<Root> = Vec::new();
    for r in roots {
        let base = r
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "root".to_string());
        let mut name = base.clone();
        let mut n = 2;
        while out.iter().any(|x| x.name == name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        out.push(Root {
            name,
            path: r.clone(),
        });
    }
    out
}

/// Every file under `dir`, as (relative path, absolute path).
fn files_under(dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    for entry in walkdir::WalkDir::new(dir)
        .min_depth(1)
        .follow_links(false)
        .sort_by_file_name()
    {
        let Ok(e) = entry else { continue };
        if e.file_type().is_file() {
            let rel = e.path().strip_prefix(dir).unwrap_or(e.path()).to_path_buf();
            out.push((rel, e.path().to_path_buf()));
        }
    }
    out
}

fn archive_name(parts: &[&str]) -> String {
    parts.join("/")
}

/// Options a `cig parcel` run picks up: today just whether to carry the
/// seal key inside the parcel (issue #105's `--with-key`).
#[derive(Clone, Debug, Default)]
pub struct WriteOpts {
    /// Carry the sealing key inside the manifest so another machine can
    /// open the sealed objects (and re-seal under its own key) without
    /// setting up the keyring first. The parcel is then a secret.
    pub with_key: bool,
}

/// Write the parcel of `rec` to `out`.
pub fn write(rec: &RunRecord, journal: &Journal, out: &Path) -> io::Result<Written> {
    write_with(rec, journal, out, &WriteOpts::default())
}

/// `write`, with options: `--with-key` embeds the sealing key.
pub fn write_with(
    rec: &RunRecord,
    journal: &Journal,
    out: &Path,
    opts: &WriteOpts,
) -> io::Result<Written> {
    let run_dir = rec.dir();
    let tin_dir = super::tin::dir();
    // (name in the archive, where its bytes are now)
    let mut sources: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (rel, abs) in files_under(&run_dir) {
        let first = rel
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_default();
        // The hop reports, the tin's pending list, the git pins and a box's
        // layers are the machine's own business, not the run's record.
        if first == "hops"
            || first == "box"
            || first == super::tin::PENDING
            || first == super::git::PINS
        {
            continue;
        }
        sources.insert(
            archive_name(&["run", &rel.to_string_lossy().replace('\\', "/")]),
            abs,
        );
    }
    let mut git_blobs = 0usize;
    let mut sealed = 0usize;
    let mut temps: Vec<PathBuf> = Vec::new();
    let tin_rel = |p: &Path| -> Option<String> {
        p.strip_prefix(&tin_dir)
            .ok()
            .map(|r| archive_name(&["tin", &r.to_string_lossy().replace('\\', "/")]))
    };
    for e in journal.entries() {
        for b in e.before.iter().chain(e.took.iter()) {
            let Before::File {
                snapshot,
                sha256,
                git,
                ..
            } = &b.before
            else {
                continue;
            };
            if let Some(g) = git {
                // The bytes git held there: carried as a plain object here.
                let name = archive_name(&["git", sha256]);
                if sources.contains_key(&name) {
                    continue;
                }
                let tmp = std::env::temp_dir().join(format!(
                    ".cigparcel-{}-{}",
                    std::process::id(),
                    &sha256[..12]
                ));
                let fetched = match super::tin::object_for(sha256) {
                    Some(o) => super::tin::materialize(&o, sha256, &tmp),
                    None => super::git::materialize(&g.repo, &g.blob, sha256, &tmp),
                };
                fetched.map_err(|err| {
                    io::Error::new(
                        err.kind(),
                        format!(
                            "the bytes of {} are in git ({}, blob {}) and could not be read: {err}",
                            b.path.display(),
                            g.repo.display(),
                            &g.blob[..g.blob.len().min(12)]
                        ),
                    )
                })?;
                temps.push(tmp.clone());
                sources.insert(name, tmp);
                git_blobs += 1;
                continue;
            }
            if snapshot.as_os_str().is_empty() {
                continue;
            }
            if super::tin::is_recipe(snapshot) {
                if let Some(name) = tin_rel(snapshot) {
                    sources.insert(name, snapshot.clone());
                }
                if let Ok(r) = super::tin::read_recipe(snapshot) {
                    for c in &r.chunks {
                        let o = super::tin::object_file(&c.sha256);
                        if let Some(name) = tin_rel(&o) {
                            sources.insert(name, o);
                        }
                    }
                }
            } else if super::tin::is_sealed(snapshot) {
                if let Some(name) = tin_rel(snapshot) {
                    if sources.insert(name, snapshot.clone()).is_none() {
                        sealed += 1;
                    }
                }
            } else if super::tin::holds(snapshot) || super::tin::is_packed(snapshot) {
                if let Some(name) = tin_rel(snapshot) {
                    sources.insert(name, snapshot.clone());
                }
            }
        }
    }
    // Sealed snapshots in the run directory count too.
    for name in sources.keys() {
        if name.starts_with("run/") && name.ends_with(super::seal::SUFFIX) {
            sealed += 1;
        }
    }
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    let mut total = 0u64;
    for (name, src) in &sources {
        total += fs::metadata(src)?.len();
        files.insert(name.clone(), super::journal::sha256_file(src)?);
    }
    // Key the parcel can carry (when asked, and only when there is a
    // sealed object for it to open): read it here so the Manifest has it
    // before serialisation. The hex copy is wiped as soon as the manifest
    // is serialised to bytes, so no plaintext sits in memory beyond that.
    //
    // A failure to read the key when `--with-key` was asked is an error:
    // writing a parcel without the key and not saying so would silently
    // make an unopenable parcel, and `--with-key` only fits when the key
    // is at hand (finding 11).
    let sealing_key = if opts.with_key && sealed > 0 {
        match super::seal::key() {
            Ok((k, _)) => Some(hex::encode(k.as_bytes())),
            Err(e) => {
                return Err(io::Error::other(format!(
                    "--with-key cannot read the tin's seal key ({e}); the parcel was not written"
                )))
            }
        }
    } else {
        None
    };
    let manifest = Manifest {
        format: FORMAT,
        cig: crate::VERSION.to_string(),
        run: rec.id.clone(),
        created: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        origin: Origin {
            state_dir: super::runs::home_dir(),
            cwd: rec.cwd.clone(),
            host: hostname(),
        },
        roots: root_names(&rec.pack),
        files,
        sealed: SealedNote {
            objects: sealed,
            key: if sealed > 0 {
                super::seal::key()
                    .ok()
                    .map(|(k, _)| super::seal::fingerprint(&k))
            } else {
                None
            },
        },
        git_blobs,
        prior: path_map_of(&run_dir),
        sealing_key,
    };
    let tmp = out.with_extension(format!("{EXT}.tmp"));
    // A pre-existing `.tmp` from a crashed earlier write could be a file
    // another user owned or whose mode the umask set non-0600: remove it
    // first and use `create_new`, so the parcel is always written to a
    // fresh file with the mode we set and nobody else's byte hitches a
    // ride (SEC-004 L3).
    let _ = fs::remove_file(&tmp);
    let mut manifest_sha256 = String::new();
    let written = (|| -> io::Result<()> {
        // A parcel carrying its own sealing key is a secret: create 0600 so
        // another user cannot read the manifest while it is being written.
        // A plain parcel keeps 0600 too — the sealed objects inside are a
        // secret even without a carried key (finding 6).
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let file = opts.open(&tmp)?;
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        let mut tar = tar::Builder::new(enc);
        let mut manifest_json = serde_json::to_vec_pretty(&manifest)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let mut header = tar::Header::new_gnu();
        header.set_size(manifest_json.len() as u64);
        header.set_mode(0o600);
        header.set_mtime(0);
        header.set_cksum();
        tar.append_data(&mut header, "manifest.json", manifest_json.as_slice())?;
        manifest_sha256 = {
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(&manifest_json))
        };
        // The in-memory manifest bytes held the hex key once a --with-key
        // parcel was asked for: wipe them. The archive on disk is the
        // secret by design (that was the ask).
        if manifest.sealing_key.is_some() {
            super::seal::wipe_bytes(&mut manifest_json);
        }
        for (name, src) in &sources {
            let mut f = fs::File::open(src)?;
            let len = f.metadata()?.len();
            let mut header = tar::Header::new_gnu();
            header.set_size(len);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            tar.append_data(&mut header, name.as_str(), &mut f)?;
        }
        let enc = tar.into_inner()?;
        let file = enc.finish()?;
        file.sync_all()?;
        Ok(())
    })();
    for t in &temps {
        let _ = fs::remove_file(t);
    }
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::rename(&tmp, out)?;
    let bytes = fs::metadata(out).map(|m| m.len()).unwrap_or(total);
    Ok(Written {
        path: out.to_path_buf(),
        files: sources.len(),
        bytes,
        manifest,
        manifest_sha256,
    })
}

/// How an import places the run here.
#[derive(Clone, Debug, Default)]
pub struct Mapping {
    /// `--root name=path`.
    pub roots: Vec<(String, PathBuf)>,
    /// `--cwd path`, when it cannot be told from the roots.
    pub cwd: Option<PathBuf>,
    /// Import even what this machine's key cannot open.
    pub allow_sealed_missing: bool,
    /// The parcel carries its own sealing key: honour it only when the
    /// caller said to (`--accept-key` on `cig unparcel`). Without the
    /// flag a `--with-key` parcel is refused, with the fingerprint and
    /// the flag shown, so a malicious parcel cannot slip a key of its
    /// own in (finding 8).
    pub accept_key: bool,
}

/// What an import did.
#[derive(Debug, Serialize)]
pub struct Imported {
    pub id: String,
    pub origin: String,
    pub files: usize,
    pub path_map: Vec<(PathBuf, PathBuf)>,
    pub sealed_unopenable: bool,
    /// When the parcel carried its own sealing key and the caller
    /// accepted it: the fingerprint computed from the CARRIED key
    /// bytes, so an operator can match it against what the sender
    /// expected (SEC-004 M2). The key itself never leaves this
    /// process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_key_fingerprint: Option<String>,
}

/// Read the manifest alone, for `cig unparcel --show` and the messages.
pub fn read_manifest(file: &Path) -> io::Result<Manifest> {
    let f = fs::File::open(file)?;
    let dec = flate2::read::GzDecoder::new(f);
    let mut archive = tar::Archive::new(dec);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.to_string_lossy() == "manifest.json" {
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            return serde_json::from_str(&text)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "no manifest.json: not a parcel",
    ))
}

/// The SHA-256 of a parcel's `manifest.json`, as its pass names it.
pub fn manifest_sha256(file: &Path) -> io::Result<String> {
    use sha2::{Digest, Sha256};
    let f = fs::File::open(file)?;
    let dec = flate2::read::GzDecoder::new(f);
    let mut archive = tar::Archive::new(dec);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.to_string_lossy() == "manifest.json" {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            let digest = hex::encode(Sha256::digest(&bytes));
            super::seal::wipe_bytes(&mut bytes);
            return Ok(digest);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "no manifest.json: not a parcel",
    ))
}

/// Why an import was refused: one line, with what to do.
pub type Refusal = String;

/// One-word label for a tar entry type, for refusal messages.
fn entry_type_label(t: tar::EntryType) -> &'static str {
    match t {
        tar::EntryType::Symlink => "symlink",
        tar::EntryType::Link => "hardlink",
        tar::EntryType::Char => "character-device",
        tar::EntryType::Block => "block-device",
        tar::EntryType::Fifo => "fifo",
        _ => "non-regular",
    }
}

/// Import a parcel. Everything is checked before anything is written, and
/// a refusal leaves the state directory as it was.
pub fn import(file: &Path, mapping: &Mapping) -> Result<Imported, Refusal> {
    let runs_dir = super::runs::runs_dir();
    fs::create_dir_all(&runs_dir)
        .map_err(|e| format!("cannot create {}: {e}", runs_dir.display()))?;
    let scratch = runs_dir.join(format!(".unparcel-{}", std::process::id()));
    let _ = fs::remove_dir_all(&scratch);
    let result = import_inner(file, mapping, &scratch);
    // Before cleaning up, overwrite the scratch manifest.json: a
    // --with-key parcel wrote its sealing key in there, and the
    // directory's remove-tree leaves the inode's bytes for the
    // allocator to hand out again (SEC-004 L3, M2).
    let manifest_path = scratch.join("manifest.json");
    let _ = super::rekey::wipe_and_remove_file(&manifest_path);
    let _ = fs::remove_dir_all(&scratch);
    result
}

fn import_inner(file: &Path, mapping: &Mapping, scratch: &Path) -> Result<Imported, Refusal> {
    // 1. Extract to scratch, beside the runs, so the final step is a rename.
    fs::create_dir_all(scratch).map_err(|e| e.to_string())?;
    let f = fs::File::open(file).map_err(|e| format!("cannot open {}: {e}", file.display()))?;
    let dec = flate2::read::GzDecoder::new(f);
    let mut archive = tar::Archive::new(dec);
    archive.set_overwrite(false);
    // Unpack one entry at a time so a non-regular entry is refused BEFORE
    // it lands on disk (SEC-004 N1(a)): a hostile parcel whose
    // `manifest.json` is a symlink to a user file (the tin's seal.key,
    // say) would otherwise be followed by the later read, and the
    // cleanup wipe would zero the target. A symlink, hardlink, device
    // node or FIFO entry in a parcel has no legitimate use — the writer
    // (parcel.rs:390-416) only ever emits regular files for the
    // manifest and the pack's contents — so we refuse the whole parcel
    // and the state directory stays as it was.
    for entry in archive
        .entries()
        .map_err(|e| format!("{} is not a parcel this cig can read: {e}", file.display()))?
    {
        let mut entry = entry
            .map_err(|e| format!("{} is not a parcel this cig can read: {e}", file.display()))?;
        let ety = entry.header().entry_type();
        if !matches!(ety, tar::EntryType::Regular | tar::EntryType::Directory) {
            let name = entry
                .path()
                .ok()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<unreadable name>".to_string());
            return Err(format!(
                "the parcel holds {name}, a {kind} entry; a parcel may only name regular files and directories, refusing the whole parcel",
                kind = entry_type_label(ety),
            ));
        }
        entry
            .unpack_in(scratch)
            .map_err(|e| format!("{} is not a parcel this cig can read: {e}", file.display()))?;
    }
    let manifest_text = fs::read_to_string(scratch.join("manifest.json"))
        .map_err(|_| format!("{} has no manifest.json: not a parcel", file.display()))?;
    let mut manifest: Manifest = serde_json::from_str(&manifest_text)
        .map_err(|e| format!("the manifest of {} does not parse: {e}", file.display()))?;
    // The manifest text held the carried sealing key in hex (SEC-004 M2):
    // wipe the buffer before letting it go. The in-memory manifest keeps
    // only the field, which we wipe after use below.
    let mut manifest_text_bytes = manifest_text.into_bytes();
    super::seal::wipe_bytes(&mut manifest_text_bytes);
    drop(manifest_text_bytes);
    if manifest.format > FORMAT {
        return Err(format!(
            "the parcel is format {}, written by cig {}; this cig ({}) reads up to format {FORMAT}: ciginstaller update, or cig versions add {}",
            manifest.format,
            manifest.cig,
            crate::VERSION,
            manifest.cig
        ));
    }
    // 2. The run must not be here already.
    let local_run = super::runs::runs_dir().join(&manifest.run);
    if local_run.exists() {
        return Err(format!(
            "run {} is already here ({}); nothing was imported",
            manifest.run,
            local_run.display()
        ));
    }
    // 3. Every file, present and hashing as the manifest says.
    let mut seen = 0usize;
    for (name, expect) in &manifest.files {
        let p = scratch.join(name);
        if !p.is_file() {
            return Err(format!(
                "the parcel's manifest names {name}, which is not in the archive; refusing the whole parcel"
            ));
        }
        let actual = super::journal::sha256_file(&p).map_err(|e| format!("{name}: {e}"))?;
        if actual != *expect {
            return Err(format!(
                "{name} does not hash as the manifest says (sha256 {} recorded, {} in the archive); the parcel was changed after it was written, refusing the whole of it",
                &expect[..expect.len().min(12)],
                &actual[..12]
            ));
        }
        seen += 1;
    }
    for (rel, _) in files_under(scratch) {
        let name = rel.to_string_lossy().replace('\\', "/");
        if name != "manifest.json" && !manifest.files.contains_key(&name) {
            return Err(format!(
                "the archive holds {name}, which the manifest does not name; refusing the whole parcel"
            ));
        }
    }
    // 4. Every root mapped, by name.
    let mut path_map: Vec<(PathBuf, PathBuf)> = Vec::new();
    for root in &manifest.roots {
        let Some((_, here)) = mapping.roots.iter().find(|(n, _)| *n == root.name) else {
            return Err(format!(
                "root `{}` ({} on the machine that ran it) is not mapped to a place here; --root {}=/path/to/it",
                root.name,
                root.path.display(),
                root.name
            ));
        };
        let here = super::normalize_root(&here.to_string_lossy());
        if !here.is_dir() {
            return Err(format!(
                "--root {}={}: that is not a directory here",
                root.name,
                here.display()
            ));
        }
        path_map.push((root.path.clone(), here));
    }
    // 5. The working directory: given, or told from a root under it.
    let origin_cwd = PathBuf::from(&manifest.origin.cwd);
    let local_cwd = match &mapping.cwd {
        Some(c) => super::normalize_root(&c.to_string_lossy()),
        None => {
            let derived = manifest
                .roots
                .iter()
                .zip(&path_map)
                .find_map(|(root, (_, here))| {
                    let rel = root.path.strip_prefix(&origin_cwd).ok()?;
                    if rel.as_os_str().is_empty() {
                        return Some(here.clone());
                    }
                    here.ends_with(rel).then(|| {
                        let mut h = here.clone();
                        for _ in rel.components() {
                            h.pop();
                        }
                        h
                    })
                });
            match derived {
                Some(d) => d,
                None if manifest.origin.cwd.is_empty() => PathBuf::new(),
                None => {
                    return Err(format!(
                        "the run's working directory ({}) cannot be told from the roots; --cwd /path/to/it",
                        manifest.origin.cwd
                    ))
                }
            }
        }
    };
    if !local_cwd.as_os_str().is_empty() && !local_cwd.is_dir() {
        return Err(format!(
            "--cwd {}: that is not a directory here",
            local_cwd.display()
        ));
    }
    if !manifest.origin.cwd.is_empty() && origin_cwd != local_cwd {
        path_map.push((origin_cwd.clone(), local_cwd.clone()));
    }
    // 6. Sealed objects: can this machine open them?
    //
    // Four paths:
    //   a. the parcel's local key already opens them (the common case);
    //   b. the parcel carries its sealing key (`--with-key`) AND the
    //      caller said `--accept-key`, so the carried key is trusted;
    //   c. the caller asked for `--allow-sealed-missing`, in which case
    //      the sealed objects are imported as opaque bytes (unburn will
    //      refuse to use them);
    //   d. refusal, with a clear message.
    //
    // Whichever path lands on the carried key validates EVERY sealed
    // object against it before anything is written: one sample passing
    // is not enough, a malicious parcel could slip a planted object
    // sealed under another key alongside benign ones (finding 8).
    let mut sealed_unopenable = false;
    let mut carried_key: Option<super::seal::Key> = None;
    let mut carried_fingerprint: Option<String> = None;
    if manifest.sealed.objects > 0 {
        let sealed_names: Vec<&String> = manifest
            .files
            .keys()
            .filter(|n| n.starts_with("tin/sealed/") || n.ends_with(super::seal::SUFFIX))
            .collect();
        let sample = sealed_names.first().map(|n| scratch.join(n));
        if let Some(p) = sample {
            let local_opens = super::seal::opens(&p, &super::seal::aad_of(&p)).is_ok();
            if !local_opens {
                // Decode the carried key first (if any), before we decide
                // whether to refuse. The fingerprint shown on refusal and
                // on accept is computed from the carried KEY, not from
                // the manifest's own `sealed.key` claim (SEC-004 M2).
                let mut decoded: Option<super::seal::Key> = None;
                if let Some(hex_key) = manifest.sealing_key.as_deref() {
                    if let Ok(mut bytes) = hex::decode(hex_key) {
                        let key = super::seal::Key::try_from(bytes.as_slice()).ok();
                        // The hex-decoded Vec held the key for a moment.
                        super::seal::wipe_bytes(&mut bytes);
                        decoded = key;
                    }
                }
                let fingerprint = decoded.as_ref().map(super::seal::fingerprint);
                if manifest.sealing_key.is_some() {
                    if !mapping.accept_key {
                        return Err(format!(
                            "the parcel carries its own sealing key (fingerprint {}); accepting it opens the parcel's sealed objects on this machine, so it is refused unless you pass --accept-key on cig unparcel",
                            fingerprint.as_deref().unwrap_or("not a valid key")
                        ));
                    }
                    if let Some(k) = decoded {
                        // Validate EVERY sealed object against the
                        // carried key, not only the sample.
                        for name in &sealed_names {
                            let p = scratch.join(name);
                            if super::seal::opens_with(&p, &super::seal::aad_of(&p), &k).is_err() {
                                return Err(format!(
                                    "the parcel carries a sealing key, but {name} does not open under it; refusing the whole parcel (one malicious sealed object would land in the tin and later refuse to restore)"
                                ));
                            }
                        }
                        carried_key = Some(k);
                        carried_fingerprint = fingerprint;
                    } else {
                        return Err(
                            "the parcel's sealing_key field is not a valid hex key; refusing the whole parcel".to_string(),
                        );
                    }
                }
                if carried_key.is_none() {
                    if !mapping.allow_sealed_missing {
                        let mine = super::seal::key()
                            .ok()
                            .map(|(k, _)| super::seal::fingerprint(&k))
                            .unwrap_or_else(|| "none".to_string());
                        return Err(format!(
                            "the parcel holds {} sealed object{} sealed under key {}; this machine's key ({mine}) does not open them. Copy that key here (the keyring entry cigscript/tin-seal, or ~/.cigscript/tin/seal.key), parcel --with-key on the other machine with --accept-key here, or --allow-sealed-missing to import what the rest can restore",
                            manifest.sealed.objects,
                            if manifest.sealed.objects == 1 { "" } else { "s" },
                            manifest.sealed.key.as_deref().unwrap_or("?")
                        ));
                    }
                    sealed_unopenable = true;
                }
            }
        }
    }
    // The in-memory manifest.sealing_key string still holds the hex key:
    // wipe its backing bytes before we let it go (SEC-004 M2, L3). The
    // Manifest value itself will be dropped later, but by then the
    // string has been zeroed.
    if let Some(s) = manifest.sealing_key.as_mut() {
        // SAFETY: wipe_bytes takes &mut [u8] from the String's bytes; we
        // then clear the String so UTF-8 invariants are kept.
        super::seal::wipe_bytes(unsafe { s.as_bytes_mut() });
        s.clear();
    }
    manifest.sealing_key = None;
    // 7. The state directories: the origin's run and tin become ours.
    let origin_state = &manifest.origin.state_dir;
    path_map.push((
        origin_state.join("runs").join(&manifest.run),
        local_run.clone(),
    ));
    path_map.push((origin_state.join("tin"), super::tin::dir()));
    // A run imported once before: its journal names the paths of the
    // machine that ran it, so those map here too, through the machine this
    // parcel came from.
    if !manifest.prior.is_empty() {
        let mut sorted = path_map.clone();
        sorted.sort_by_key(|(from, _)| std::cmp::Reverse(from.as_os_str().len()));
        for (older, mid) in &manifest.prior {
            if path_map.iter().any(|(from, _)| from == older) {
                continue;
            }
            path_map.push((older.clone(), remap(mid, &sorted)));
        }
    }
    // 8. Objects into the tin: content-named ones are checked against their
    // name, a stand-in for a git blob is placed as a whole object. A
    // sealed object under `tin/sealed/` with a carried key is re-sealed
    // under this machine's key on the way in, so it opens later without
    // the carried key at hand.
    //
    // The tin key's lock before any object is placed (SEC-004 L4, U1), so
    // a `cig tin rekey` cannot slip between the reads and the writes
    // below; where it cannot be had, the import refuses before it writes
    // anything. The lock alone: a parcel with nothing sealed asks for no
    // key (#129).
    super::seal::hold_shared_rekey_lock()?;
    let tin_dir = super::tin::dir();
    for name in manifest.files.keys() {
        let src = scratch.join(name);
        let (dest, reseal_sealed) = if let Some(rel) = name.strip_prefix("tin/") {
            (tin_dir.join(rel), name.starts_with("tin/sealed/"))
        } else if let Some(sha) = name.strip_prefix("git/") {
            (super::tin::object_path_pub(sha), false)
        } else {
            continue;
        };
        if dest.is_file() {
            // Same name, same bytes by construction; a corrupt one is replaced.
            if let Some(expected) = dest
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .filter(|n| n.len() == 64)
            {
                if super::journal::sha256_file(&dest).ok().as_deref() == Some(expected.as_str()) {
                    continue;
                }
            } else {
                continue;
            }
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let tmp = dest.with_extension("unparcel");
        if reseal_sealed {
            if let Some(from) = carried_key.as_ref() {
                let aad = super::seal::aad_of(&src);
                super::seal::reseal(&src, &tmp, &aad, from).map_err(|e| {
                    format!(
                        "re-sealing sealed object {} under this machine's key failed: {e}",
                        src.display()
                    )
                })?;
            } else {
                fs::copy(&src, &tmp).map_err(|e| e.to_string())?;
            }
        } else {
            fs::copy(&src, &tmp).map_err(|e| e.to_string())?;
        }
        super::journal::sync_file(&tmp).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o444));
        }
        if dest.exists() {
            let _ = fs::remove_file(&dest);
        }
        fs::rename(&tmp, &dest).map_err(|e| e.to_string())?;
    }
    // 9. The run directory, with the record saying where it came from.
    let staged_run = scratch.join("run");
    let record_path = staged_run.join("run.json");
    let text = fs::read_to_string(&record_path).map_err(|e| format!("run.json: {e}"))?;
    let mut rec: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("run.json: {e}"))?;
    rec["cwd"] = serde_json::Value::String(local_cwd.to_string_lossy().to_string());
    rec["pack"] = serde_json::to_value(
        path_map
            .iter()
            .take(manifest.roots.len())
            .map(|(_, to)| to)
            .collect::<Vec<_>>(),
    )
    .unwrap_or_default();
    rec["pid"] = serde_json::Value::Null;
    rec["pid_started"] = serde_json::Value::Null;
    let origin = format!(
        "{}{} ({})",
        if manifest.origin.host.is_empty() {
            String::new()
        } else {
            format!("{}:", manifest.origin.host)
        },
        origin_state.display(),
        manifest.created
    );
    rec["imported"] = serde_json::json!({
        "from": origin,
        "at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "parcel": file.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        "path_map": path_map,
    });
    fs::write(
        &record_path,
        serde_json::to_string_pretty(&rec).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    // When the parcel carried its sealing key, the sealed snapshots the
    // run keeps are re-sealed under this machine's key IN THE STAGED
    // directory, before the rename places the run. A reseal that fails
    // after the rename left a half-converted run behind, and a re-import
    // was then refused as a duplicate; staging first lets the whole
    // import be atomic (finding 8).
    if let Some(from) = carried_key.as_ref() {
        let snaps = staged_run.join("snapshots");
        if let Ok(rd) = fs::read_dir(&snaps) {
            for e in rd.flatten() {
                let p = e.path();
                if p.file_name()
                    .map(|n| n.to_string_lossy().ends_with(super::seal::SUFFIX))
                    .unwrap_or(false)
                {
                    let aad = super::seal::aad_of(&p);
                    super::seal::reseal(&p, &p, &aad, from).map_err(|err| {
                        format!(
                            "re-sealing snapshot {} under this machine's key failed: {err}",
                            p.display()
                        )
                    })?;
                }
            }
        }
    }
    fs::rename(&staged_run, &local_run)
        .map_err(|e| format!("cannot place the run at {}: {e}", local_run.display()))?;
    let _ = super::journal::sync_dir_pub(&super::runs::runs_dir());
    // The journal came signed under the other machine's key (or unsigned,
    // from an older cig). Signed again under this machine's key, with the
    // record's head following it, so from here on it verifies like any run
    // made here; its origin goes in the ledger.
    if let Ok(Some(head)) = super::journal::resign_in_place(&local_run) {
        let mut rec = rec;
        rec["journal_head"] = serde_json::Value::String(head.clone());
        if let Ok(text) = serde_json::to_string_pretty(&rec) {
            let _ = fs::write(local_run.join("run.json"), text);
        }
        let lines = super::journal::Journal::load(&local_run)
            .map(|j| j.chain().lines as u64)
            .unwrap_or(0);
        let status = rec["status"].as_str().unwrap_or("").to_string();
        let _ = super::ledger::append("origin", &manifest.run, &head, &status, lines);
    }
    Ok(Imported {
        id: manifest.run.clone(),
        origin,
        files: seen,
        path_map,
        sealed_unopenable,
        accepted_key_fingerprint: carried_fingerprint,
    })
}

/// The path map an imported run's record carries, longest origin first,
/// so the most specific mapping wins.
pub fn path_map_of(run_dir: &Path) -> Vec<(PathBuf, PathBuf)> {
    let Ok(text) = fs::read_to_string(run_dir.join("run.json")) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut map: Vec<(PathBuf, PathBuf)> = v
        .get("imported")
        .and_then(|i| i.get("path_map"))
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();
    map.sort_by_key(|(from, _)| std::cmp::Reverse(from.as_os_str().len()));
    map
}

/// `path` as it is here: the longest mapped origin prefix replaced. A path
/// that is relative (as a script wrote it), or under no mapped prefix, is
/// itself. "Rooted" rather than "absolute": a Unix path read on Windows
/// has a root and no drive, and must still be mapped.
pub fn remap(path: &Path, map: &[(PathBuf, PathBuf)]) -> PathBuf {
    if map.is_empty() || !path.has_root() {
        return path.to_path_buf();
    }
    for (from, to) in map {
        if let Ok(rest) = path.strip_prefix(from) {
            return if rest.as_os_str().is_empty() {
                to.clone()
            } else {
                to.join(rest)
            };
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_are_named_and_paths_remap_by_longest_prefix() {
        let roots = root_names(&[
            PathBuf::from("/srv/app/build"),
            PathBuf::from("/srv/other/build"),
            PathBuf::from("/"),
        ]);
        let names: Vec<&str> = roots.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["build", "build-2", "root"]);
        let map = vec![
            (PathBuf::from("/srv"), PathBuf::from("/home/me")),
            (
                PathBuf::from("/srv/app/build"),
                PathBuf::from("/home/me/proj/build"),
            ),
        ];
        let mut sorted = map.clone();
        sorted.sort_by_key(|(from, _)| std::cmp::Reverse(from.as_os_str().len()));
        assert_eq!(
            remap(Path::new("/srv/app/build/x.txt"), &sorted),
            PathBuf::from("/home/me/proj/build/x.txt")
        );
        assert_eq!(
            remap(Path::new("/srv/app/other.txt"), &sorted),
            PathBuf::from("/home/me/app/other.txt")
        );
        assert_eq!(remap(Path::new("rel/x"), &sorted), PathBuf::from("rel/x"));
        assert_eq!(
            remap(Path::new("/elsewhere"), &sorted),
            PathBuf::from("/elsewhere")
        );
    }
}
