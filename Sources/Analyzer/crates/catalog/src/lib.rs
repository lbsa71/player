//! Single-writer JSON catalogue. Readers need no lock and retain immutable references.
pub mod sidecars;
mod read_snapshot;
pub use read_snapshot::ReadSnapshotStats;
use fs2::FileExt;
use mixlib_audio::WavReader;
use mixlib_types::*;
use serde::de::DeserializeOwned;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};
use tempfile::NamedTempFile;
use walkdir::WalkDir;

#[derive(Clone)]
pub struct Store {
    pub root: PathBuf,
    read_cache: Option<std::sync::Arc<read_snapshot::ReadSnapshot>>,
}
pub struct Writer {
    file: File,
}
impl Drop for Writer {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}
impl Store {
    pub fn open(root: &Path) -> Result<Self> {
        let s = Self {
            root: fs::canonicalize(root)?,
            read_cache: None,
        };
        s.read::<Library>("library.json")?;
        Ok(s)
    }
    pub fn init(root: &Path) -> Result<Self> {
        Self::init_layout(root, MetadataLayout::SidecarV1)
    }
    /// For opening/testing the previous layout and explicit migration.
    pub fn init_legacy(root: &Path) -> Result<Self> {
        Self::init_layout(root, MetadataLayout::SeparateV1)
    }
    fn init_layout(root: &Path, metadata_layout: MetadataLayout) -> Result<Self> {
        fs::create_dir_all(root)?;
        require(
            fs::read_dir(root)?.next().is_none(),
            "LIBRARY_NOT_EMPTY",
            "initialize an empty metadata directory",
        )?;
        let s = Self {
            root: fs::canonicalize(root)?,
            read_cache: None,
        };
        let _lock = s.lock(false, &Cancellation::default())?;
        require(
            !s.root.join("library.json").exists(),
            "LIBRARY_EXISTS",
            "library already initialized",
        )?;
        for d in [
            "config",
            "models",
            "tracks",
            "catalog",
            "transitions",
            "plans",
            "jobs",
            "reports",
            "staging",
            "locks",
        ] {
            s.directory(d)?;
        }
        s.write("config/analysis.json", &AnalysisConfig::default())?;
        s.write("config/mixing.json", &MixingConfig::default())?;
        s.write(
            "models/manifest.json",
            &ModelManifest {
                header: Header::default(),
                manifest_id: "models".into(),
                models: vec![],
            },
        )?;
        let library = Library {
            header: Header::default(),
            library_id: id(),
            metadata_layout,
            source_roots: vec![],
            configuration_refs: BTreeMap::from([
                ("analysis".into(), "config/analysis.json".into()),
                ("mixing".into(), "config/mixing.json".into()),
            ]),
            excluded_directories: vec![],
            created_at: now(),
        };
        s.write("library.json", &library)?;
        if library.metadata_layout == MetadataLayout::SidecarV1 {
            s.write(
                "catalog/sidecars.json",
                &SidecarIndex {
                    header: Header::default(),
                    index_id: "sidecars".into(),
                    tracks: BTreeMap::new(),
                },
            )?;
        }
        Ok(s)
    }
    /// Lexical traversal checks plus canonical containment checks on every existing component.
    pub fn path(&self, relative: &str) -> Result<PathBuf> {
        let p = Path::new(relative);
        require(
            !relative.is_empty() && p.components().all(|c| matches!(c, Component::Normal(_))),
            "PATH_TRAVERSAL",
            format!("unsafe metadata reference: {relative}"),
        )?;
        let mut out = self.root.clone();
        for c in p.components() {
            out.push(c);
            if fs::symlink_metadata(&out).is_ok() {
                require(
                    !fs::symlink_metadata(&out)?.file_type().is_symlink(),
                    "PATH_SYMLINK",
                    format!("symlink in metadata path: {relative}"),
                )?;
                require(
                    fs::canonicalize(&out)?.starts_with(&self.root),
                    "PATH_TRAVERSAL",
                    relative,
                )?;
            }
        }
        Ok(out)
    }
    pub fn directory(&self, relative: &str) -> Result<PathBuf> {
        require(self.read_cache.is_none(), "READ_SNAPSHOT_WRITE", "metadata snapshot cannot create directories or acquire a writer lock")?;
        let p = self.path(relative)?;
        fs::create_dir_all(&p)?;
        Ok(p)
    }
    pub fn read<T: Record>(&self, relative: &str) -> Result<T> {
        validate_value(&serde_json::from_slice(&self.record_bytes(relative)?)?)
    }
    pub fn record_bytes(&self, relative: &str) -> Result<Vec<u8>> {
        if self.read_cache.is_some() { return self.snapshot_record_bytes(relative); }
        if let Some(owner) = sidecars::record_owner(relative)? {
            if self.uses_sidecars()? {
                return self.sidecar_record(&owner, relative);
            }
        }
        read_limited(&self.path(relative)?)
    }
    pub fn record_exists(&self, relative: &str) -> Result<bool> {
        if let Some(owner) = sidecars::record_owner(relative)? {
            if self.uses_sidecars()? {
                let bytes = if self.read_cache.is_some() { self.snapshot_record_bytes(relative) } else { self.sidecar_record(&owner, relative) };
                return match bytes {
                    Ok(_) => Ok(true),
                    Err(e) if e.code == "RECORD_MISSING" => Ok(false),
                    Err(e) => Err(e),
                };
            }
        }
        Ok(self.path(relative)?.exists())
    }
    pub fn read_ref<T: Record>(&self, r: &FileRef) -> Result<T> {
        let bytes = self.record_bytes(&r.path)?;
        require(
            hash_bytes(&bytes) == r.hash,
            "CHECKSUM",
            format!("hash mismatch: {}", r.path),
        )?;
        validate_value(&serde_json::from_slice(&bytes)?)
    }
    pub fn verify_ref(&self, r: &FileRef) -> Result<()> {
        let bytes = self.record_bytes(&r.path)?;
        require(hash_bytes(&bytes) == r.hash, "CHECKSUM", &r.path)
    }
    pub fn write<T: Record>(&self, relative: &str, record: &T) -> Result<FileRef> {
        require(self.read_cache.is_none(), "READ_SNAPSHOT_WRITE", "metadata snapshot is read-only; publish through the live store after verification")?;
        let bytes = encode(record)?;
        if let Some(owner) = sidecars::record_owner(relative)? {
            if self.uses_sidecars()? {
                self.write_sidecar_record(&owner, relative, &bytes)?;
                return Ok(FileRef {
                    path: relative.into(),
                    hash: hash_bytes(&bytes),
                });
            }
        }
        let path = self.path(relative)?;
        let parent = path.parent().unwrap();
        fs::create_dir_all(parent)?;
        self.path(relative)?;
        let mut temp = NamedTempFile::new_in(parent)?;
        temp.write_all(&bytes)?;
        temp.as_file().sync_all()?;
        read_record::<T>(temp.path())?;
        temp.persist(&path).map_err(|e| Error::from(e.error))?;
        File::open(parent)?.sync_all()?;
        Ok(FileRef {
            path: relative.into(),
            hash: hash_bytes(&bytes),
        })
    }
    pub fn lock(&self, wait: bool, cancel: &Cancellation) -> Result<Writer> {
        self.directory("locks")?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.path("locks/writer.lock.json")?)?;
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => break,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    require(
                        wait,
                        "WRITER_BUSY",
                        "another writer owns this library; use --wait-lock to wait",
                    )?;
                    cancel.check()?;
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => return Err(e.into()),
            }
        }
        let mut record = Report::new("writer_lock");
        record
            .header
            .extensions
            .insert("pid".into(), serde_json::json!(std::process::id()));
        file.seek(SeekFrom::Start(0))?;
        file.set_len(0)?;
        file.write_all(&encode(&record)?)?;
        file.sync_all()?;
        Ok(Writer { file })
    }
    pub fn track_path(track: &str) -> Result<String> {
        valid_id(track)?;
        Ok(format!("tracks/{}/{track}/track.json", shard(track)))
    }
    pub fn track(&self, id: &str) -> Result<Track> {
        self.read(&Self::track_path(id)?)
    }
    pub fn save_track(&self, t: &Track) -> Result<()> {
        self.write(&Self::track_path(&t.track_id)?, t)?;
        Ok(())
    }
    pub fn records<T: Record>(&self, dir: &str, filename: Option<&str>) -> Result<Vec<T>> {
        let mut paths = vec![];
        for entry in WalkDir::new(self.path(dir)?).follow_links(false) {
            let e = entry.map_err(|e| Error::new("WALK", e.to_string()))?;
            if e.file_type().is_file()
                && e.path().extension().is_some_and(|x| x == "json")
                && filename.is_none_or(|f| e.file_name() == f)
            {
                paths.push(e.path().to_path_buf());
            }
        }
        paths.sort();
        paths.iter().map(|p| read_record(p)).collect()
    }
    pub fn tracks(&self) -> Result<Vec<Track>> {
        if self.uses_sidecars()? {
            return self
                .sidecar_index()?
                .tracks
                .keys()
                .map(|id| self.track(id))
                .collect();
        }
        let mut tracks = self.records::<Track>("tracks", Some("track.json"))?;
        tracks.sort_by(|a, b| a.track_id.cmp(&b.track_id));
        Ok(tracks)
    }
    pub fn source(&self, t: &Track) -> Result<PathBuf> {
        let lib: Library = self.read("library.json")?;
        for loc in t.source_locations.iter().filter(|l| l.available) {
            if let Some(root) = lib.source_roots.iter().find(|r| r.root_id == loc.root_id) {
                let rel = Path::new(&loc.relative_path);
                require(
                    !rel.is_absolute()
                        && rel.components().all(|c| matches!(c, Component::Normal(_))),
                    "PATH_TRAVERSAL",
                    "unsafe source location",
                )?;
                let base = Path::new(&root.path);
                let joined = base.join(rel);
                if let Ok(p) = fs::canonicalize(&joined) {
                    require(
                        p.starts_with(fs::canonicalize(base)?),
                        "PATH_TRAVERSAL",
                        "source escapes registered root",
                    )?;
                    if p.is_file() {
                        return Ok(p);
                    }
                }
            }
        }
        Err(Error::new(
            "SOURCE_MISSING",
            format!("no available source for {}", t.track_id),
        ))
    }
    pub fn job(&self, t: &Track) -> Result<()> {
        let path = format!("jobs/{}.json", t.track_id);
        let old = if self.record_exists(&path)? {
            Some(self.read::<Job>(&path)?)
        } else {
            None
        };
        if old.as_ref().is_some_and(|j| {
            j.audio_hash == t.audio_hash && matches!(j.state, JobState::Queued | JobState::Running)
        }) {
            return Ok(());
        }
        let job = Job {
            header: Header::default(),
            job_id: t.track_id.clone(),
            track_id: t.track_id.clone(),
            audio_hash: t.audio_hash.clone(),
            state: JobState::Queued,
            stage: "analysis".into(),
            retry_count: old.map_or(0, |j| j.retry_count + 1),
            diagnostic: None,
            updated_at: now(),
        };
        self.save_job(&job)
    }
    pub fn save_job(&self, job: &Job) -> Result<()> {
        valid_id(&job.job_id)?;
        self.write(&format!("jobs/{}.json", job.job_id), job)?;
        Ok(())
    }
    pub fn report(&self, r: &Report) -> Result<()> {
        self.write(&format!("reports/{}.json", r.report_id), r)?;
        Ok(())
    }
    pub fn analysis(&self, t: &Track) -> Result<AnalysisManifest> {
        let a = self.read_ref::<AnalysisManifest>(
            t.active_analysis
                .as_ref()
                .ok_or_else(|| Error::new("ANALYSIS_MISSING", &t.track_id))?,
        )?;
        require(a.audio_hash == t.audio_hash, "ANALYSIS_STALE", &t.track_id)?;
        Ok(a)
    }
    pub fn overrides(&self, t: &Track) -> Result<Overrides> {
        if let Some(reference) = &t.active_overrides {
            let o: Overrides = self.read_ref(reference)?;
            require(
                o.revision == t.override_revision,
                "OVERRIDE_REVISION",
                "correction revision mismatch",
            )?;
            return Ok(o);
        }
        let p = format!(
            "tracks/{}/{}/overrides.json",
            shard(&t.track_id),
            t.track_id
        );
        if self.record_exists(&p)? {
            let o: Overrides = self.read(&p)?;
            require(
                o.revision == t.override_revision,
                "OVERRIDE_REVISION",
                "correction and track revisions differ",
            )?;
            Ok(o)
        } else {
            require(t.override_revision == 0, "OVERRIDE_MISSING", &t.track_id)?;
            Ok(Overrides {
                header: Header::default(),
                overrides_id: t.track_id.clone(),
                revision: 0,
                base_audio_hash: t.audio_hash.clone(),
                base_analysis: t.active_analysis.clone(),
                operations: vec![],
                user_preferences: BTreeMap::new(),
                migration_status: "current".into(),
            })
        }
    }
    pub fn set_overrides(&self, track_id: &str, mut o: Overrides) -> Result<()> {
        let mut t = self.track(track_id)?;
        require(
            o.base_audio_hash == t.audio_hash,
            "OVERRIDE_AUDIO",
            "corrections must target current original audio",
        )?;
        require(
            o.base_analysis.as_ref().map(|r| &r.hash)
                == t.active_analysis.as_ref().map(|r| &r.hash),
            "OVERRIDE_ANALYSIS",
            "corrections must target current analysis",
        )?;
        o.revision = t.override_revision + 1;
        o.migration_status = "current".into();
        o.overrides_id = t.track_id.clone();
        // Publish an immutable correction snapshot before swapping the track pointer.
        // overrides.json is a convenience copy; snapshot readers never observe mixed revisions.
        t.active_overrides = Some(self.write(
            &format!(
                "tracks/{}/{}/overrides/{}-{}.json",
                shard(&t.track_id),
                t.track_id,
                o.revision,
                id()
            ),
            &o,
        )?);
        t.override_revision = o.revision;
        self.save_track(&t)?;
        self.write(
            &format!(
                "tracks/{}/{}/overrides.json",
                shard(&t.track_id),
                t.track_id
            ),
            &o,
        )?;
        Ok(())
    }
}
pub fn shard(id: &str) -> String {
    hash_bytes(id.as_bytes()).digest[..2].into()
}
pub fn read_limited(path: &Path) -> Result<Vec<u8>> {
    let f = File::open(path)?;
    require(
        f.metadata()?.len() <= MAX_JSON,
        "RECORD_TOO_LARGE",
        path.display().to_string(),
    )?;
    let mut bytes = vec![];
    f.take(MAX_JSON + 1).read_to_end(&mut bytes)?;
    require(
        bytes.len() as u64 <= MAX_JSON,
        "RECORD_TOO_LARGE",
        "record grew while reading",
    )?;
    Ok(bytes)
}
pub fn read_record<T: Record>(path: &Path) -> Result<T> {
    validate_value(&serde_json::from_slice(&read_limited(path)?)?)
}
pub fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&read_limited(path)?)?)
}
pub fn fingerprint(path: &Path) -> Result<Fingerprint> {
    let m = fs::metadata(path)?;
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        format!("{}:{}", m.dev(), m.ino())
    };
    #[cfg(not(unix))]
    let identity = String::new();
    Ok(Fingerprint {
        size: m.len(),
        modified_ns: m
            .modified()?
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
            .to_string(),
        file_identity: identity,
    })
}

pub fn scan(
    store: &Store,
    sources: &[PathBuf],
    verify: bool,
    cancel: &Cancellation,
) -> Result<Report> {
    let mut library: Library = store.read("library.json")?;
    let config: AnalysisConfig = store.read("config/analysis.json")?;
    let mut report = Report::new("scan");
    for key in [
        "unchanged",
        "queued",
        "deferred",
        "unavailable",
        "failed",
        "new",
        "moved",
        "duplicate",
        "metadata_changed",
        "audio_changed",
    ] {
        report.counts.insert(key.into(), 0);
    }
    for source in sources {
        let p = fs::canonicalize(source)?;
        require(
            p.is_dir() && !p.starts_with(&store.root),
            "SOURCE_ROOT",
            "source must be a directory outside the metadata directory",
        )?;
        if !library.source_roots.iter().any(|r| Path::new(&r.path) == p) {
            library.source_roots.push(SourceRoot {
                root_id: id(),
                display_name: p.file_name().unwrap_or_default().to_string_lossy().into(),
                location_kind: "local_absolute".into(),
                path: p.to_string_lossy().into(),
                available: true,
                require_finalized_sidecar: default_audio_directory()
                    .and_then(|audio| Ok(fs::canonicalize(audio)?))
                    .is_ok_and(|audio| p == audio),
            });
        }
    }
    require(
        !library.source_roots.is_empty(),
        "SOURCE_ROOT",
        "no sources registered; pass --source",
    )?;
    store.write("library.json", &library)?;
    let mut tracks = store.tracks()?;
    let old = tracks.clone();
    let mut paths: BTreeMap<(String, String), usize> = BTreeMap::new();
    for (i, t) in old.iter().enumerate() {
        for l in &t.source_locations {
            paths.insert((l.root_id.clone(), l.relative_path.clone()), i);
        }
    }
    let mut excluded = vec![store.root.clone()];
    for p in &library.excluded_directories {
        excluded.push(PathBuf::from(p));
    }
    let mut candidates = vec![];
    for root in &mut library.source_roots {
        cancel.check()?;
        root.available = Path::new(&root.path).is_dir();
        for t in &mut tracks {
            for l in &mut t.source_locations {
                if l.root_id == root.root_id {
                    l.available = false;
                }
            }
        }
        if !root.available {
            report.diagnostics.push(Diagnostic {
                code: "ROOT_UNAVAILABLE".into(),
                subject: root.root_id.clone(),
                message: root.path.clone(),
            });
            continue;
        }
        for entry in WalkDir::new(&root.path)
            .follow_links(false)
            .into_iter()
            .filter_entry(|e| !excluded.iter().any(|p| e.path().starts_with(p)))
        {
            cancel.check()?;
            match entry {
                Ok(e)
                    if e.file_type().is_file()
                        && e.path()
                            .extension()
                            .is_some_and(|x| x.to_string_lossy().eq_ignore_ascii_case("wav")) =>
                {
                    let rel = e
                        .path()
                        .strip_prefix(&root.path)
                        .unwrap()
                        .to_string_lossy()
                        .to_string();
                    match fingerprint(e.path()) {
                        Ok(fp) => {
                            candidates.push((root.root_id.clone(), rel, e.path().to_path_buf(), fp))
                        }
                        Err(e) => {
                            report.count("failed");
                            report.error(&rel, &e);
                        }
                    }
                }
                Err(e) => {
                    report.count("failed");
                    report.error(&root.path, &Error::new("WALK", e.to_string()));
                }
                _ => {}
            }
        }
    }
    candidates.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    // One settling interval for the batch. Stable discovery hints can bypass it.
    let need_settle = candidates.iter().any(|(r, p, _, fp)| {
        paths.get(&(r.clone(), p.clone())).is_none_or(|&i| {
            old[i]
                .source_locations
                .iter()
                .find(|l| l.root_id == *r && l.relative_path == *p)
                .unwrap()
                .fingerprint
                != *fp
        })
    });
    if need_settle {
        let deadline = std::time::Instant::now() + Duration::from_millis(config.settling_ms);
        while std::time::Instant::now() < deadline {
            cancel.check()?;
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    for (root, rel, path, before) in candidates {
        cancel.check()?;
        let needs_finalized = library
            .source_roots
            .iter()
            .find(|r| r.root_id == root)
            .is_some_and(|r| r.require_finalized_sidecar);
        if let Err(e) = sidecars::admit_wav(&path, needs_finalized) {
            report.count(if e.code == "RECORDING_IN_PROGRESS" {
                "deferred"
            } else {
                "failed"
            });
            report.error(&rel, &e);
            continue;
        }
        let key = (root.clone(), rel.clone());
        let existing = paths.get(&key).copied();
        let previous = existing.map(|i| &old[i]);
        let unchanged = previous
            .and_then(|t| {
                t.source_locations
                    .iter()
                    .find(|l| l.root_id == root && l.relative_path == rel)
            })
            .is_some_and(|l| l.fingerprint == before);
        if unchanged && !verify {
            let i = existing.unwrap();
            if let Some(l) = tracks[i]
                .source_locations
                .iter_mut()
                .find(|l| l.root_id == root && l.relative_path == rel)
            {
                l.available = true;
            }
            if tracks[i].active_analysis.is_none() {
                store.job(&tracks[i])?;
                report.count("queued");
            } else {
                report.count("unchanged");
            }
            continue;
        }
        let result = (|| -> Result<(AudioFormat, ContentHash, ContentHash)> {
            require(
                fingerprint(&path)? == before,
                "SOURCE_SETTLING",
                "source changed during settling interval",
            )?;
            let mut wav = WavReader::open(&path)?;
            let (whole, audio) = wav.hashes(cancel)?;
            require(
                fingerprint(&path)? == before,
                "SOURCE_SETTLING",
                "source changed during hashing",
            )?;
            Ok((wav.format, whole, audio))
        })();
        let (format, whole, audio) = match result {
            Ok(v) => v,
            Err(e) => {
                if e.code == "CANCELLED" {
                    return Err(e);
                }
                report.count(if e.code == "SOURCE_SETTLING" {
                    "deferred"
                } else {
                    "failed"
                });
                report.error(&rel, &e);
                if let Some(i) = existing {
                    store.job(&tracks[i])?;
                    let mut job: Job = store.read(&format!("jobs/{}.json", tracks[i].track_id))?;
                    job.state = if e.code == "SOURCE_SETTLING" {
                        JobState::Deferred
                    } else {
                        JobState::Failed
                    };
                    job.diagnostic = Some(e.to_string());
                    store.save_job(&job)?;
                }
                continue;
            }
        };
        let matching = tracks.iter().position(|t| t.audio_hash == audio);
        let target = if let Some(i) = matching {
            if existing != Some(i) {
                let had_live = tracks[i].source_locations.iter().any(|l| {
                    l.available
                        || library
                            .source_roots
                            .iter()
                            .find(|r| r.root_id == l.root_id)
                            .is_some_and(|r| Path::new(&r.path).join(&l.relative_path).is_file())
                });
                report.count(if had_live { "duplicate" } else { "moved" });
            } else if previous.is_some_and(|t| {
                t.source_locations
                    .iter()
                    .any(|l| l.root_id == root && l.relative_path == rel && l.file_hash != whole)
            }) {
                report.count("metadata_changed");
            }
            i
        } else if let Some(i) = existing.filter(|&i| tracks[i].source_locations.len() == 1) {
            tracks[i].audio_hash = audio.clone();
            tracks[i].audio_revision += 1;
            tracks[i].format = format.clone();
            tracks[i].active_analysis = None;
            report.count("audio_changed");
            i
        } else {
            let t = Track {
                header: Header::default(),
                track_id: id(),
                audio_revision: 1,
                audio_hash: audio.clone(),
                format,
                source_locations: vec![],
                active_analysis: None,
                active_overrides: None,
                override_revision: 0,
                lifecycle: Lifecycle::Available,
            };
            tracks.push(t);
            report.count("new");
            tracks.len() - 1
        };
        if let Some(i) = existing {
            tracks[i]
                .source_locations
                .retain(|l| !(l.root_id == root && l.relative_path == rel));
        }
        tracks[target]
            .source_locations
            .retain(|l| !(l.root_id == root && l.relative_path == rel));
        tracks[target].source_locations.push(SourceLocation {
            root_id: root.clone(),
            relative_path: rel.clone(),
            fingerprint: before,
            file_hash: whole,
            available: true,
        });
        store.attach_sidecar(
            &tracks[target],
            &SidecarLocation {
                root_id: root,
                relative_wav_path: rel,
            },
        )?;
        store.save_track(&tracks[target])?;
        if tracks[target].active_analysis.is_none() {
            store.job(&tracks[target])?;
            report.count("queued");
        } else {
            report.count("unchanged");
        }
    }
    for t in &mut tracks {
        if t.source_locations.is_empty() {
            // Keep a detached historical location for identity/history, explicitly unavailable.
            if let Some(old) = old.iter().find(|o| o.track_id == t.track_id) {
                t.source_locations = old.source_locations.clone();
                for l in &mut t.source_locations {
                    l.available = false;
                }
            }
        }
        let available = t.source_locations.iter().any(|l| l.available);
        t.lifecycle = if available {
            Lifecycle::Available
        } else {
            report.count("unavailable");
            Lifecycle::Unavailable
        };
        store.save_track(t)?;
    }
    store.write("library.json", &library)?;
    store.report(&report)?;
    rebuild_catalog(store)?;
    Ok(report)
}

pub fn rebuild_catalog(store: &Store) -> Result<CatalogManifest> {
    if store.uses_sidecars()? {
        store.rebuild_sidecar_index()?;
    }
    let generation = id();
    let mut shards: BTreeMap<String, Vec<CatalogSummary>> = BTreeMap::new();
    for t in store.tracks()? {
        shards
            .entry(shard(&t.track_id))
            .or_default()
            .push(CatalogSummary {
                track_id: t.track_id,
                audio_hash: t.audio_hash,
                duration_seconds: t.format.frames as f64 / t.format.sample_rate_hz as f64,
                available: matches!(t.lifecycle, Lifecycle::Available),
                analysis: t.active_analysis,
                override_revision: t.override_revision,
            });
    }
    let mut refs = vec![];
    for (s, tracks) in shards {
        refs.push(store.write(
            &format!("catalog/{generation}/{s}.json"),
            &CatalogShard {
                header: Header::default(),
                shard_id: s,
                tracks,
            },
        )?);
    }
    let manifest = CatalogManifest {
        header: Header::default(),
        generation_id: generation,
        shards: refs,
    };
    store.write("catalog/manifest.json", &manifest)?;
    Ok(manifest)
}
pub fn validate(store: &Store, cancel: &Cancellation) -> Result<Report> {
    let mut report = Report::new("validate");
    let _: AnalysisConfig = store.read("config/analysis.json")?;
    let _: MixingConfig = store.read("config/mixing.json")?;
    let models: ModelManifest = store.read("models/manifest.json")?;
    for m in models.models {
        let p = store.path(&m.file.path)?;
        let bytes = fs::read(p)?;
        require(
            hash_bytes(&bytes) == m.file.hash,
            "MODEL_CHECKSUM",
            &m.model_id,
        )?;
    }
    for t in store.tracks()? {
        cancel.check()?;
        let result = (|| -> Result<()> {
            let o = store.overrides(&t)?;
            if !o.operations.is_empty() {
                require(
                    o.base_audio_hash == t.audio_hash,
                    "OVERRIDE_UNRESOLVED",
                    &t.track_id,
                )?;
            }
            if let Some(r) = &t.active_analysis {
                let a: AnalysisManifest = store.read_ref(r)?;
                require(a.audio_hash == t.audio_hash, "ANALYSIS_STALE", &t.track_id)?;
                validate_analysis(store, &a)?;
            }
            if matches!(t.lifecycle, Lifecycle::Available) {
                let mut w = WavReader::open(&store.source(&t)?)?;
                require(
                    w.hashes(cancel)?.1 == t.audio_hash,
                    "SOURCE_CHANGED",
                    &t.track_id,
                )?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => report.count("valid_tracks"),
            Err(e) => {
                report.count("failed");
                report.error(&t.track_id, &e);
            }
        }
    }
    for p in store.records::<MixPlan>("plans", None)? {
        match verify_plan(store, &p, cancel) {
            Ok(_) => report.count("valid_plans"),
            Err(e) => {
                report.count("failed");
                report.error(&p.plan_id, &e);
            }
        }
    }
    if store.path("catalog/manifest.json")?.exists() {
        let c: CatalogManifest = store.read("catalog/manifest.json")?;
        for r in c.shards {
            match store.read_ref::<CatalogShard>(&r) {
                Ok(_) => report.count("valid_shards"),
                Err(e) => {
                    report.count("failed");
                    report.error(&r.path, &e);
                }
            }
        }
    } else {
        report.diagnostics.push(Diagnostic {
            code: "CATALOG_MISSING".into(),
            subject: "catalog".into(),
            message: "run rebuild-catalog; authoritative records are intact".into(),
        });
    }
    Ok(report)
}
pub fn validate_analysis(store: &Store, a: &AnalysisManifest) -> Result<()> {
    for name in ["beats", "harmony", "regions"] {
        require(
            a.artifacts.contains_key(name),
            "ANALYSIS_ARTIFACT",
            format!("missing {name} artifact"),
        )?;
    }
    for (name, r) in &a.artifacts {
        match name.as_str() {
            "beats" => {
                store.read_ref::<Beats>(r)?;
            }
            "harmony" => {
                let h: Harmony = store.read_ref(r)?;
                for r in h.feature_chunks {
                    store.read_ref::<FeatureChunk>(&r)?;
                }
            }
            "regions" => {
                store.read_ref::<Regions>(r)?;
            }
            "segments" => {
                store.read_ref::<SegmentAnalysis>(r)?;
            }
            "tactic_analysis" => {
                store.read_ref::<TacticTrackAnalysis>(r)?;
            }
            _ => store.verify_ref(r)?,
        }
    }
    for stage in a.stages.values() {
        for r in stage.artifacts.values() {
            store.verify_ref(r)?;
        }
    }
    Ok(())
}
pub fn verify_plan(
    store: &Store,
    p: &MixPlan,
    cancel: &Cancellation,
) -> Result<BTreeMap<String, PathBuf>> {
    p.validate()?;
    let mut sources = BTreeMap::new();
    for placement in &p.placements {
        cancel.check()?;
        let t = store.track(&placement.track_id)?;
        require(
            t.audio_hash == placement.audio_hash && t.audio_revision == placement.audio_revision,
            "SOURCE_CHANGED",
            &t.track_id,
        )?;
        require(
            t.override_revision == placement.override_revision,
            "OVERRIDE_CHANGED",
            format!("{} corrections changed; replan", t.track_id),
        )?;
        let a: AnalysisManifest = store.read_ref(&placement.analysis)?;
        require(a.audio_hash == t.audio_hash, "ANALYSIS_STALE", &t.track_id)?;
        validate_analysis(store, &a)?;
        let source = store.source(&t)?;
        let mut wav = WavReader::open(&source)?;
        require(
            wav.hashes(cancel)?.1 == placement.audio_hash,
            "SOURCE_CHANGED",
            &t.track_id,
        )?;
        sources.insert(t.track_id, source);
    }
    for (key, audio) in p.prepared_assets() {
        let path = store.path(&audio.path)?;
        require(
            WavReader::open(&path)?.hashes(cancel)?.0 == audio.hash,
            "DRUM_INTRO_CHANGED",
            &audio.path,
        )?;
        sources.insert(key.into(), path);
    }
    for tactic in read_tactics(p)? {
        for asset in tactic.stem_assets {
            let path=store.path(&asset.audio.path)?;
            require(WavReader::open(&path)?.hashes(cancel)?.0==asset.audio.hash,
                "TACTIC_STEM_CHANGED", &asset.audio.path)?;
            sources.insert(asset.audio.path, path);
        }
    }
    Ok(sources)
}

pub fn default_audio_directory() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| Error::new("HOME_MISSING", "set --source explicitly"))?;
    Ok(PathBuf::from(home).join("Documents/Audio"))
}
pub fn default_library_directory() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| Error::new("HOME_MISSING", "set --library explicitly"))?;
    Ok(PathBuf::from(home).join("Library/Application Support/Player/Analyzer"))
}
