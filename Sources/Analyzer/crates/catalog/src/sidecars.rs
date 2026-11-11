//! One physical JSON sidecar per WAV. The recorder owns its top-level fields;
//! this adapter alone owns `analyzer` and the resolution of logical record keys.
use crate::*;
use serde_json::{Map, Value};

pub const MAX_SIDECAR: u64 = 256 * 1024 * 1024;
const INDEX: &str = "catalog/sidecars.json";

pub fn record_owner(key: &str) -> Result<Option<String>> {
    require(
        Path::new(key)
            .components()
            .all(|c| matches!(c, Component::Normal(_))),
        "PATH_TRAVERSAL",
        key,
    )?;
    let parts: Vec<_> = key.split('/').collect();
    let owner = match parts.as_slice() {
        ["tracks", _, track, ..] => Some((*track).to_string()),
        ["jobs", job] => job.strip_suffix(".json").map(str::to_string),
        ["staging", track, ..] => Some((*track).to_string()),
        _ => None,
    };
    if let Some(id) = &owner {
        valid_id(id)?;
    }
    Ok(owner)
}

pub fn sidecar_path(wav: &Path) -> PathBuf {
    let mut path = wav.as_os_str().to_os_string();
    path.push(".json");
    PathBuf::from(path)
}

pub fn read_document(path: &Path) -> Result<Map<String, Value>> {
    require(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "PATH_SYMLINK",
        "sidecar must not be a symlink",
    )?;
    let file = File::open(path)?;
    require(
        file.metadata()?.len() <= MAX_SIDECAR,
        "SIDECAR_TOO_LARGE",
        "sidecar exceeds 256 MiB; previous metadata retained",
    )?;
    let mut bytes = vec![];
    file.take(MAX_SIDECAR + 1).read_to_end(&mut bytes)?;
    require(
        bytes.len() as u64 <= MAX_SIDECAR,
        "SIDECAR_TOO_LARGE",
        "sidecar grew while reading",
    )?;
    let value: Value = serde_json::from_slice(&bytes)?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| Error::new("SIDECAR_FORMAT", "WAV sidecar must be a JSON object"))
}

fn bundle(document: &Map<String, Value>) -> Result<Option<SidecarBundle>> {
    document
        .get("analyzer")
        .map(validate_value::<SidecarBundle>)
        .transpose()
}

fn publish(path: &Path, document: &Map<String, Value>) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(document)?;
    require(
        bytes.len() as u64 <= MAX_SIDECAR,
        "SIDECAR_TOO_LARGE",
        "sidecar exceeds 256 MiB; previous metadata retained",
    )?;
    let parent = path
        .parent()
        .ok_or_else(|| Error::new("SIDECAR_PATH", "missing sidecar parent"))?;
    let mut temp = NamedTempFile::new_in(parent)?;
    temp.write_all(&bytes)?;
    temp.as_file().sync_all()?;
    let checked = read_document(temp.path())?;
    bundle(&checked)?.ok_or_else(|| Error::new("SIDECAR_FORMAT", "missing analyzer object"))?;
    temp.persist(path).map_err(|e| Error::from(e.error))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

impl Store {
    pub fn uses_sidecars(&self) -> Result<bool> {
        Ok(self.read::<Library>("library.json")?.metadata_layout == MetadataLayout::SidecarV1)
    }

    fn resolve_location(&self, location: &SidecarLocation) -> Result<PathBuf> {
        let library: Library = self.read("library.json")?;
        let root = library
            .source_roots
            .iter()
            .find(|r| r.root_id == location.root_id)
            .ok_or_else(|| {
                Error::new("SOURCE_ROOT", "sidecar references an unknown source root")
            })?;
        let relative = Path::new(&location.relative_wav_path);
        require(
            !relative.as_os_str().is_empty()
                && relative
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))),
            "PATH_TRAVERSAL",
            "unsafe sidecar location",
        )?;
        let root = fs::canonicalize(&root.path)?;
        let wav = root.join(relative);
        let parent = fs::canonicalize(wav.parent().unwrap())?;
        require(
            parent.starts_with(&root),
            "PATH_TRAVERSAL",
            "sidecar parent escapes source root",
        )?;
        let path = sidecar_path(&parent.join(wav.file_name().unwrap()));
        if let Ok(m) = fs::symlink_metadata(&path) {
            require(
                !m.file_type().is_symlink(),
                "PATH_SYMLINK",
                "sidecar must not be a symlink",
            )?;
        }
        Ok(path)
    }

    /// Index is disposable. Recover it by inspecting sidecars, without changing them.
    pub fn sidecar_index(&self) -> Result<SidecarIndex> {
        let path = self.path(INDEX)?;
        if path.exists() {
            let index: SidecarIndex = self.read(INDEX)?;
            for (track, loc) in &index.tracks {
                if !self.resolve_location(loc)?.exists() {
                    let recovered = self.discover_sidecars()?;
                    require(
                        index
                            .tracks
                            .keys()
                            .all(|id| recovered.tracks.contains_key(id)),
                        "SIDECAR_MISSING",
                        format!("missing metadata for {track}; restore the WAV's sidecar"),
                    )?;
                    return Ok(recovered);
                }
            }
            return Ok(index);
        }
        self.discover_sidecars()
    }

    fn discover_sidecars(&self) -> Result<SidecarIndex> {
        let library: Library = self.read("library.json")?;
        let previous = read_record::<SidecarIndex>(&self.path(INDEX)?).ok();
        let mut index = SidecarIndex {
            header: Header::default(),
            index_id: "sidecars".into(),
            tracks: BTreeMap::new(),
        };
        for root in &library.source_roots {
            if !Path::new(&root.path).is_dir() {
                continue;
            }
            for entry in WalkDir::new(&root.path).follow_links(false) {
                let e = entry.map_err(|e| Error::new("WALK", e.to_string()))?;
                let name = e.file_name().to_string_lossy().to_lowercase();
                if !e.file_type().is_file()
                    || !name.ends_with(".wav.json")
                    || name.ends_with(".partial.wav.json")
                {
                    continue;
                }
                let document = match read_document(e.path()) {
                    Ok(doc) => doc,
                    Err(err) => {
                        // A malformed newly discovered recorder sidecar is reported by
                        // scan admission. Never silently discard an indexed owner.
                        let known = previous.as_ref().is_none_or(|old| {
                            old.tracks
                                .values()
                                .any(|loc| self.resolve_location(loc).is_ok_and(|p| p == e.path()))
                        });
                        if known {
                            return Err(err);
                        }
                        continue;
                    }
                };
                if let Some(b) = bundle(&document)? {
                    if b.library_id == library.library_id && b.shared_from.is_none() {
                        let relative = e.path().strip_prefix(&root.path).unwrap().to_string_lossy();
                        let location = SidecarLocation {
                            root_id: root.root_id.clone(),
                            relative_wav_path: relative.strip_suffix(".json").unwrap().into(),
                        };
                        require(
                            index.tracks.insert(b.track_id, location).is_none(),
                            "SIDECAR_ID_CONFLICT",
                            "two sidecars claim the same primary track; relink explicitly",
                        )?;
                    }
                }
            }
        }
        Ok(index)
    }

    pub fn rebuild_sidecar_index(&self) -> Result<()> {
        let index = self.discover_sidecars()?;
        self.write(INDEX, &index)?;
        Ok(())
    }

    pub fn sidecar_for_track(&self, track: &str) -> Result<PathBuf> {
        let index = self.sidecar_index()?;
        let loc = index.tracks.get(track).ok_or_else(|| {
            Error::new(
                "SIDECAR_MISSING",
                format!("no sidecar for {track}; scan or rebuild-catalog"),
            )
        })?;
        self.resolve_location(loc)
    }

    pub(crate) fn sidecar_record(&self, owner: &str, key: &str) -> Result<Vec<u8>> {
        let document = read_document(&self.sidecar_for_track(owner)?)?;
        let b = bundle(&document)?.ok_or_else(|| Error::new("SIDECAR_MISSING", owner))?;
        require(
            b.track_id == owner
                && b.library_id == self.read::<Library>("library.json")?.library_id
                && b.shared_from.is_none(),
            "SIDECAR_OWNER",
            "sidecar owner does not match catalogue",
        )?;
        let value = b
            .records
            .get(key)
            .ok_or_else(|| Error::new("RECORD_MISSING", key))?;
        let bytes = serde_json::to_vec_pretty(value)?;
        require(
            bytes.len() as u64 <= logical_json_limit(key),
            "RECORD_TOO_LARGE",
            key,
        )?;
        Ok(bytes)
    }

    pub(crate) fn write_sidecar_record(&self, owner: &str, key: &str, bytes: &[u8]) -> Result<()> {
        require(
            self.read_cache.is_none(),
            "READ_SNAPSHOT_WRITE",
            "metadata snapshot cannot publish sidecar records",
        )?;
        let path = self.sidecar_for_track(owner)?;
        // Directory advisory lock coordinates sidecar writers across library instances,
        // without adding another file alongside every WAV. The recorder writes once,
        // before our finalized-sidecar admission check, and never rewrites that file.
        let directory = File::open(path.parent().unwrap())?;
        directory
            .try_lock_exclusive()
            .map_err(|e| Error::new("SIDECAR_BUSY", e.to_string()))?;
        let mut document = read_document(&path)?;
        let mut b = bundle(&document)?.ok_or_else(|| Error::new("SIDECAR_MISSING", owner))?;
        require(
            b.track_id == owner
                && b.library_id == self.read::<Library>("library.json")?.library_id
                && b.shared_from.is_none(),
            "SIDECAR_OWNER",
            "sidecar owner changed",
        )?;
        let value = serde_json::from_slice(bytes)?;
        // No write needed on unchanged scans; preserve both file contents and mtime.
        if b.records.get(key) == Some(&value) {
            return Ok(());
        }
        b.records.insert(key.into(), value);
        document.insert("analyzer".into(), serde_json::to_value(b)?);
        publish(&path, &document)
    }

    /// Attach a discovered WAV to one canonical bundle. Duplicate audio has one
    /// analysis owner and a lightweight shared_from reference in its own sidecar.
    pub(crate) fn attach_sidecar(&self, track: &Track, location: &SidecarLocation) -> Result<()> {
        require(
            self.read_cache.is_none(),
            "READ_SNAPSHOT_WRITE",
            "metadata snapshot cannot attach sidecars",
        )?;
        if !self.uses_sidecars()? {
            return Ok(());
        }
        let mut index = self.sidecar_index()?;
        let path = self.resolve_location(location)?;
        let directory = File::open(path.parent().unwrap())?;
        directory
            .try_lock_exclusive()
            .map_err(|e| Error::new("SIDECAR_BUSY", e.to_string()))?;
        let mut document = if path.exists() {
            read_document(&path)?
        } else {
            Map::new()
        };
        let library: Library = self.read("library.json")?;
        let mut existing = bundle(&document)?;
        if let Some(b) = &existing {
            require(
                b.library_id == library.library_id,
                "SIDECAR_OWNER",
                "sidecar belongs to another library; metadata retained",
            )?;
            if b.track_id != track.track_id {
                require(b.shared_from.is_some(), "SIDECAR_OWNER", "a primary WAV changed identity while sharing analysis; original metadata retained; restore that WAV before rescanning")?;
                // An alias owns no analysis history. The original primary keeps
                // every old generation when this duplicate's audio changes.
                existing = None;
            }
        }
        let primary = index
            .tracks
            .get(&track.track_id)
            .cloned()
            .unwrap_or_else(|| location.clone());
        let mut b = existing.unwrap_or(SidecarBundle {
            header: Header::default(),
            bundle_id: id(),
            library_id: library.library_id,
            track_id: track.track_id.clone(),
            shared_from: None,
            records: BTreeMap::new(),
        });
        if primary != *location {
            b.shared_from = Some(primary.clone());
        } else {
            b.records.insert(
                Self::track_path(&track.track_id)?,
                serde_json::to_value(track)?,
            );
        }
        let value = serde_json::to_value(b)?;
        if document.get("analyzer") != Some(&value) {
            document.insert("analyzer".into(), value);
            publish(&path, &document)?;
        }
        if index.tracks.get(&track.track_id) != Some(&primary) {
            index.tracks.insert(track.track_id.clone(), primary);
            self.write(INDEX, &index)?;
        }
        Ok(())
    }

    pub fn recording_metadata(&self, track: &Track) -> Result<Value> {
        let path = if self.uses_sidecars()? {
            self.sidecar_for_track(&track.track_id)?
        } else {
            sidecar_path(&self.source(track)?)
        };
        if !path.exists() {
            return Ok(Value::Null);
        }
        let mut doc = read_document(&path)?;
        doc.remove("analyzer");
        Ok(Value::Object(doc))
    }

    /// Preserve exact record hashes and saved-plan references when fusing a v1
    /// separate-file library. Publication precedes removal of legacy JSON files.
    pub fn fuse_sidecars(&self) -> Result<Report> {
        require(
            self.read_cache.is_none(),
            "READ_SNAPSHOT_WRITE",
            "metadata snapshot cannot migrate sidecars",
        )?;
        let mut library: Library = self.read("library.json")?;
        if library.metadata_layout == MetadataLayout::SidecarV1 {
            return self.finish_fusion();
        }
        let tracks = self.tracks()?;
        let mut prepared = vec![];
        // Preflight every track before mutating any recorder metadata.
        for t in &tracks {
            let wav = self.source(t)?;
            for loc in &t.source_locations {
                if loc.available {
                    let root = library
                        .source_roots
                        .iter()
                        .find(|r| r.root_id == loc.root_id)
                        .ok_or_else(|| Error::new("SOURCE_ROOT", &loc.root_id))?;
                    let source = Path::new(&root.path).join(&loc.relative_path);
                    admit_wav(&source, root.require_finalized_sidecar)?;
                    let sidecar = sidecar_path(&source);
                    if sidecar.exists() {
                        if let Some(b) = bundle(&read_document(&sidecar)?)? {
                            require(
                                b.library_id == library.library_id && b.track_id == t.track_id,
                                "SIDECAR_OWNER",
                                "migration would replace another analyzer's metadata",
                            )?;
                        }
                    }
                }
            }
            let path = sidecar_path(&wav);
            let location = t
                .source_locations
                .iter()
                .find(|l| {
                    l.available
                        && library.source_roots.iter().any(|r| {
                            r.root_id == l.root_id
                                && Path::new(&r.path).join(&l.relative_path) == wav
                        })
                })
                .ok_or_else(|| Error::new("SOURCE_MISSING", &t.track_id))?;
            let mut doc = if path.exists() {
                read_document(&path)?
            } else {
                Map::new()
            };
            let mut records = BTreeMap::new();
            let mut originals = vec![];
            for prefix in [
                format!("tracks/{}/{}", shard(&t.track_id), t.track_id),
                format!("staging/{}", t.track_id),
            ] {
                let base = self.path(&prefix)?;
                if !base.exists() {
                    continue;
                }
                for e in WalkDir::new(base).follow_links(false) {
                    let e = e.map_err(|e| Error::new("WALK", e.to_string()))?;
                    if !e.file_type().is_file() || e.path().extension().is_none_or(|x| x != "json")
                    {
                        continue;
                    }
                    let bytes = read_limited(e.path())?;
                    let value: Value = serde_json::from_slice(&bytes)?;
                    require(
                        bytes == serde_json::to_vec_pretty(&value)?,
                        "MIGRATION_ENCODING",
                        "record encoding must preserve referenced byte hashes",
                    )?;
                    records.insert(
                        e.path()
                            .strip_prefix(&self.root)
                            .unwrap()
                            .to_string_lossy()
                            .into(),
                        value,
                    );
                    originals.push(e.path().to_path_buf());
                }
            }
            let job = self.path(&format!("jobs/{}.json", t.track_id))?;
            if job.exists() {
                let bytes = read_limited(&job)?;
                let value: Value = serde_json::from_slice(&bytes)?;
                require(
                    bytes == serde_json::to_vec_pretty(&value)?,
                    "MIGRATION_ENCODING",
                    "job encoding must preserve referenced byte hashes",
                )?;
                records.insert(format!("jobs/{}.json", t.track_id), value);
                originals.push(job);
            }
            if let Some(existing) = bundle(&doc)? {
                require(
                    existing.library_id == library.library_id && existing.track_id == t.track_id,
                    "SIDECAR_OWNER",
                    "migration would replace another analyzer's metadata",
                )?;
            }
            let mut b = bundle(&doc)?.unwrap_or_else(|| SidecarBundle {
                header: Header::default(),
                bundle_id: id(),
                library_id: library.library_id.clone(),
                track_id: t.track_id.clone(),
                shared_from: None,
                records: BTreeMap::new(),
            });
            b.shared_from = None;
            b.records.extend(records);
            doc.insert("analyzer".into(), serde_json::to_value(b)?);
            require(
                serde_json::to_vec_pretty(&doc)?.len() as u64 <= MAX_SIDECAR,
                "SIDECAR_TOO_LARGE",
                &t.track_id,
            )?;
            prepared.push((
                t.track_id.clone(),
                SidecarLocation {
                    root_id: location.root_id.clone(),
                    relative_wav_path: location.relative_path.clone(),
                },
                path,
                doc,
                originals,
            ));
        }
        let mut index = SidecarIndex {
            header: Header::default(),
            index_id: "sidecars".into(),
            tracks: BTreeMap::new(),
        };
        for (id, loc, path, doc, _) in &prepared {
            let directory = File::open(path.parent().unwrap())?;
            directory
                .try_lock_exclusive()
                .map_err(|e| Error::new("SIDECAR_BUSY", e.to_string()))?;
            // Merge into the latest recorder document, not the preflight copy.
            let mut latest = if path.exists() {
                read_document(path)?
            } else {
                Map::new()
            };
            latest.insert("analyzer".into(), doc["analyzer"].clone());
            publish(path, &latest)?;
            index.tracks.insert(id.clone(), loc.clone());
        }
        self.write(INDEX, &index)?;
        library.metadata_layout = MetadataLayout::SidecarV1;
        self.write("library.json", &library)?;
        let mut report = Report::new("fuse_sidecars");
        for (id, _, _, _, originals) in prepared {
            // Check the newly selected snapshot before removing each copied record.
            self.track(&id)?;
            for path in originals {
                let key = path.strip_prefix(&self.root).unwrap().to_string_lossy();
                require(
                    read_limited(&path)? == self.sidecar_record(&id, &key)?,
                    "MIGRATION_CHECKSUM",
                    &*key,
                )?;
                fs::remove_file(path)?;
            }
            report.count("fused");
        }
        // Materialize aliases for duplicate locations, retaining their own recorder fields.
        for t in tracks {
            for loc in &t.source_locations {
                if loc.available {
                    self.attach_sidecar(
                        &t,
                        &SidecarLocation {
                            root_id: loc.root_id.clone(),
                            relative_wav_path: loc.relative_path.clone(),
                        },
                    )?;
                }
            }
        }
        Ok(report)
    }

    // Retrying after the layout switch also finishes interrupted cleanup. Only
    // remove a legacy file if the authoritative sidecar contains identical bytes.
    fn finish_fusion(&self) -> Result<Report> {
        let mut report = Report::new("fuse_sidecars");
        for t in self.tracks()? {
            let doc = read_document(&self.sidecar_for_track(&t.track_id)?)?;
            let b = bundle(&doc)?.ok_or_else(|| Error::new("SIDECAR_MISSING", &t.track_id))?;
            for (key, value) in &b.records {
                require(
                    record_owner(key)?.as_deref() == Some(&t.track_id),
                    "SIDECAR_OWNER",
                    key,
                )?;
                let path = self.path(key)?;
                if path.exists() {
                    require(
                        read_limited(&path)? == serde_json::to_vec_pretty(value)?,
                        "MIGRATION_CHECKSUM",
                        key,
                    )?;
                    fs::remove_file(path)?;
                    report.count("removed_legacy_records");
                }
            }
            for loc in &t.source_locations {
                if loc.available {
                    self.attach_sidecar(
                        &t,
                        &SidecarLocation {
                            root_id: loc.root_id.clone(),
                            relative_wav_path: loc.relative_path.clone(),
                        },
                    )?;
                }
            }
        }
        Ok(report)
    }
}

/// Recorder files with unfinished state are never admitted, even if their
/// size happens to stay stable during the settling interval.
pub fn admit_wav(path: &Path, require_finalized_sidecar: bool) -> Result<()> {
    require(
        !path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase()
            .ends_with(".partial.wav"),
        "RECORDING_IN_PROGRESS",
        "partial recording deferred",
    )?;
    let sidecar = sidecar_path(path);
    if sidecar.exists() {
        let doc = read_document(&sidecar)?;
        if let Some(state) = doc.get("segmentFinalized") {
            require(
                state.as_bool() == Some(true),
                "RECORDING_IN_PROGRESS",
                "recorder has not finalized this segment",
            )?;
        } else {
            require(
                !require_finalized_sidecar,
                "RECORDING_IN_PROGRESS",
                "waiting for finalized recorder metadata",
            )?;
        }
        bundle(&doc)?;
    } else {
        require(
            !require_finalized_sidecar,
            "RECORDING_IN_PROGRESS",
            "waiting for recorder JSON publication",
        )?;
    }
    Ok(())
}
