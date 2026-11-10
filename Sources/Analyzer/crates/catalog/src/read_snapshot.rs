//! An operation-scoped, read-only view of metadata. The caller verifies the
//! physical input bytes before using the result, then returns to a fresh Store
//! for publication and source-WAV verification.
use crate::*;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub(crate) struct ReadSnapshot {
    files: Mutex<BTreeMap<PathBuf, Arc<Vec<u8>>>>,
    records: Mutex<BTreeMap<String, Arc<Vec<u8>>>>,
    bundles: Mutex<BTreeMap<PathBuf, Arc<SidecarBundle>>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadSnapshotStats {
    pub physical_files: usize,
    pub logical_records: usize,
    pub sidecar_bundles: usize,
}

fn lock<T>(value: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>> {
    value
        .lock()
        .map_err(|_| Error::new("READ_SNAPSHOT", "metadata snapshot lock poisoned"))
}

impl Store {
    /// Create an isolated lazy metadata snapshot. Clones share its cached view;
    /// the original Store remains live. Writes through this view are forbidden.
    /// Call verify_read_snapshot before using a search result, and run final
    /// source/plan verification using the original uncached Store.
    pub fn read_snapshot(&self) -> Self {
        Self {
            root: self.root.clone(),
            read_cache: Some(Arc::new(ReadSnapshot::default())),
        }
    }

    /// Reusable derived evidence must belong to this exact operation, not just
    /// another view of the same directory. Uncached stores never share a view.
    pub fn shares_read_snapshot(&self, other: &Store) -> bool {
        match (&self.read_cache, &other.read_cache) {
            (Some(a), Some(b)) => self.root == other.root && Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    /// Exact-byte revalidation detects changes during the operation, including
    /// same-size replacement, correction edits and invalid JSON. WAVs are not
    /// cached here and retain the existing export-time audio hash validation.
    pub fn verify_read_snapshot(&self, cancel: &Cancellation) -> Result<()> {
        let Some(cache) = &self.read_cache else {
            return Ok(());
        };
        let files = lock(&cache.files)?.clone();
        for (path, original) in files {
            cancel.check()?;
            let current = read_physical(&path, sidecars::MAX_SIDECAR)?;
            require(
                current == *original,
                "READ_SNAPSHOT_CHANGED",
                format!("metadata changed during search: {}", path.display()),
            )?;
        }
        Ok(())
    }

    pub fn read_snapshot_stats(&self) -> Result<Option<ReadSnapshotStats>> {
        self.read_cache
            .as_ref()
            .map(|cache| {
                Ok(ReadSnapshotStats {
                    physical_files: lock(&cache.files)?.len(),
                    logical_records: lock(&cache.records)?.len(),
                    sidecar_bundles: lock(&cache.bundles)?.len(),
                })
            })
            .transpose()
    }

    pub(crate) fn snapshot_record_bytes(&self, relative: &str) -> Result<Vec<u8>> {
        let cache = self
            .read_cache
            .as_ref()
            .ok_or_else(|| Error::new("READ_SNAPSHOT", "missing snapshot"))?;
        if let Some(bytes) = lock(&cache.records)?.get(relative).cloned() {
            return Ok((*bytes).clone());
        }
        let owner = sidecars::record_owner(relative)?;
        let sidecar_layout = owner
            .as_ref()
            .map(|_| self.uses_sidecars())
            .transpose()?
            .unwrap_or(false);
        let bytes = if let Some(owner) = owner.filter(|_| sidecar_layout) {
            let path = self.sidecar_for_track(&owner)?;
            let cached = lock(&cache.bundles)?.get(&path).cloned();
            let bundle = if let Some(bundle) = cached {
                bundle
            } else {
                let physical = self.snapshot_physical(&path, sidecars::MAX_SIDECAR)?;
                let document: serde_json::Value = serde_json::from_slice(&physical)?;
                let value = document
                    .get("analyzer")
                    .ok_or_else(|| Error::new("SIDECAR_MISSING", &owner))?;
                let bundle = Arc::new(validate_value::<SidecarBundle>(value)?);
                lock(&cache.bundles)?.insert(path, bundle.clone());
                bundle
            };
            require(
                bundle.track_id == owner
                    && bundle.library_id == self.read::<Library>("library.json")?.library_id
                    && bundle.shared_from.is_none(),
                "SIDECAR_OWNER",
                "sidecar owner does not match catalogue",
            )?;
            let value = bundle
                .records
                .get(relative)
                .ok_or_else(|| Error::new("RECORD_MISSING", relative))?;
            let bytes = serde_json::to_vec_pretty(value)?;
            require(bytes.len() as u64 <= MAX_JSON, "RECORD_TOO_LARGE", relative)?;
            bytes
        } else {
            (*self.snapshot_physical(&self.path(relative)?, MAX_JSON)?).clone()
        };
        lock(&cache.records)?.insert(relative.into(), Arc::new(bytes.clone()));
        Ok(bytes)
    }

    fn snapshot_physical(&self, path: &Path, limit: u64) -> Result<Arc<Vec<u8>>> {
        let cache = self
            .read_cache
            .as_ref()
            .ok_or_else(|| Error::new("READ_SNAPSHOT", "missing snapshot"))?;
        if let Some(bytes) = lock(&cache.files)?.get(path).cloned() {
            require(
                bytes.len() as u64 <= limit,
                "JSON_TOO_LARGE",
                path.display().to_string(),
            )?;
            return Ok(bytes);
        }
        let bytes = Arc::new(read_physical(path, limit)?);
        lock(&cache.files)?.insert(path.into(), bytes.clone());
        Ok(bytes)
    }
}

fn read_physical(path: &Path, limit: u64) -> Result<Vec<u8>> {
    require(
        !fs::symlink_metadata(path)?.file_type().is_symlink(),
        "PATH_SYMLINK",
        path.display().to_string(),
    )?;
    let file = File::open(path)?;
    require(
        file.metadata()?.len() <= limit,
        "JSON_TOO_LARGE",
        path.display().to_string(),
    )?;
    let mut bytes = vec![];
    file.take(limit + 1).read_to_end(&mut bytes)?;
    require(
        bytes.len() as u64 <= limit,
        "JSON_TOO_LARGE",
        path.display().to_string(),
    )?;
    Ok(bytes)
}
