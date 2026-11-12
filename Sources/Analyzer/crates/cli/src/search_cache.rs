//! Durable, content-addressed search evidence. No extra sidecars beside WAVs.
//! Changed admission/rendering code starts a new namespace. Set scoring and
//! scheduling retain measured decisions; completed sets are revalidated.
use super::*;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(super) struct Cache {
    root: String,
    legacy_root: String,
    pub sources: BTreeMap<String, String>,
    pub hits: usize,
    pub misses: usize,
    pub invalid: usize,
}

fn policy_key(policy: &MixingConfig) -> Result<String> {
    let mut p = policy.clone();
    // Scheduling and composition do not change pair admission. Keep every
    // unknown extension, so a newly introduced musical rule invalidates reuse.
    for k in [
        "set_config",
        "set_minutes",
        "set_structure",
        "lazy_measurement_budget",
        "lazy_search_seconds",
        "lazy_incumbent_plan",
        "required_spotify_pairs",
        "required_pair_mode",
    ] {
        p.header.extensions.remove(k);
    }
    p.beam_width = 32;
    p.max_expansions = 100000;
    Ok(canonical_hash(&p)?.digest)
}
fn source_key(track: &Track) -> Result<String> {
    Ok(
        canonical_hash(&json!({"id":track.track_id,"audio":track.audio_hash,
        "revision":track.audio_revision,"format":track.format,"analysis":track.active_analysis,
        "overrides":track.active_overrides,"override_revision":track.override_revision}))?
        .digest,
    )
}
impl Cache {
    /// Other code generations are proposal sources only, never admission
    /// evidence. Limit traversal to measured windows under this exact policy.
    pub fn previous_roots(&self, store: &Store, limit: usize) -> Result<Vec<String>> {
        let base = store.path("search-cache/v1")?;
        if !base.exists() {
            return Ok(vec![]);
        }
        let policy = self.root.rsplit('/').next().unwrap();
        let mut roots = vec![];
        for entry in std::fs::read_dir(base)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let Some(namespace) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            // Frontier/checkpoint directories are not admission namespaces.
            if namespace.len() != 64 || !namespace.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            let root = format!("search-cache/v1/{namespace}/{policy}");
            if root == self.root {
                continue;
            }
            let windows = store.path(&format!("{root}/windows"))?;
            if let Ok(meta) = windows.metadata() {
                if meta.is_dir() {
                    roots.push((meta.modified()?, root));
                }
            }
        }
        roots.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
        Ok(roots.into_iter().take(limit).map(|(_, root)| root).collect())
    }
    /// Migrate the earlier, over-specific namespace only when it matches this
    /// exact executable's musical sources. All payloads retain their checksums.
    /// Future set scoring changes reuse pair evidence and rescore full plans.
    pub fn migrate_legacy(&self, store: &Store) -> Result<()> {
        let old = store.path(&self.legacy_root)?;
        let new = store.path(&self.root)?;
        if !new.exists() && old.exists() {
            std::fs::create_dir_all(new.parent().unwrap())?;
            store.path(&self.root)?;
            std::fs::rename(old, new)?;
            eprintln!("set search: migrated compatible measured decisions to the admission cache");
        }
        Ok(())
    }
    pub fn keys(&self, store: &Store, kind: &str) -> Result<Vec<String>> {
        let path = store.path(&format!("{}/{kind}", self.root))?;
        if !path.exists() {
            return Ok(vec![]);
        }
        let mut keys = vec![];
        for entry in std::fs::read_dir(path)? {
            let path = entry?.path();
            if path.extension().is_some_and(|e| e == "json") {
                if let Some(key) = path.file_stem().and_then(|n| n.to_str()) {
                    keys.push(key.to_owned());
                }
            }
        }
        keys.sort();
        Ok(keys)
    }
    pub fn open(policy: &MixingConfig, pool: &[Track]) -> Result<Self> {
        Ok(Self {
            root: format!(
                "search-cache/v1/{}/{}",
                env!("MIXLIB_ADMISSION_RULES_HASH"),
                policy_key(policy)?
            ),
            legacy_root: format!(
                "search-cache/v1/{}/{}",
                env!("MIXLIB_MUSICAL_RULES_HASH"),
                policy_key(policy)?
            ),
            sources: pool
                .iter()
                .map(|t| Ok((t.track_id.clone(), source_key(t)?)))
                .collect::<Result<_>>()?,
            hits: 0,
            misses: 0,
            invalid: 0,
        })
    }
    pub fn pair_key(&self, from: &str, to: &str, inputs: &Value) -> Result<String> {
        Ok(canonical_hash(
            &json!({"from":self.sources[from],"to":self.sources[to],"inputs":inputs}),
        )?
        .digest)
    }
    pub fn read<T: DeserializeOwned>(
        &mut self,
        store: &Store,
        kind: &str,
        key: &str,
    ) -> Result<Option<T>> {
        let path = format!("{}/{kind}/{key}.json", self.root);
        if !store.path(&path)?.exists() {
            self.misses += 1;
            return Ok(None);
        }
        let record = store.read::<Report>(&path).and_then(|r| {
            let payload = &r.header.extensions["payload"];
            require(
                r.header.extensions.get("digest") == Some(&json!(canonical_hash(payload)?)),
                "SEARCH_CACHE",
                "cache payload checksum changed",
            )?;
            Ok(serde_json::from_value(payload.clone())?)
        });
        match record {
            Ok(value) => {
                self.hits += 1;
                Ok(Some(value))
            }
            Err(_) => {
                self.invalid += 1;
                self.misses += 1;
                Ok(None)
            }
        }
    }
    pub fn write(
        &self,
        store: &Store,
        kind: &str,
        key: &str,
        payload: &impl Serialize,
    ) -> Result<()> {
        let payload = serde_json::to_value(payload)?;
        let mut record = Report::new("set_search_cache");
        record
            .header
            .extensions
            .insert("digest".into(), json!(canonical_hash(&payload)?));
        record.header.extensions.insert("payload".into(), payload);
        store.write(&format!("{}/{kind}/{key}.json", self.root), &record)?;
        Ok(())
    }
    pub fn set_key(policy: &MixingConfig) -> Result<String> {
        let minutes = policy
            .header
            .extensions
            .get("set_minutes")
            .and_then(Value::as_u64)
            .unwrap_or(60);
        let shape = policy
            .header
            .extensions
            .get("set_structure")
            .and_then(Value::as_str)
            .unwrap_or("auto");
        let shape = if shape == "auto" {
            if minutes == 120 {
                "chapters"
            } else {
                "story"
            }
        } else {
            shape
        };
        let pairs = policy
            .header
            .extensions
            .get("required_spotify_pairs")
            .cloned()
            .unwrap_or_else(|| json!([]));
        let mode = policy
            .header
            .extensions
            .get("required_pair_mode")
            .cloned()
            .unwrap_or_else(|| json!("either"));
        Ok(
            canonical_hash(&json!({"minutes":minutes,"shape":shape,"pairs":pairs,"mode":mode}))?
                .digest,
        )
    }
    pub fn saved_set(&mut self, store: &Store, policy: &MixingConfig) -> Result<Option<MixPlan>> {
        let Some(saved) = self.read::<Value>(store, "sets", &Self::set_key(policy)?)? else {
            return Ok(None);
        };
        let sources = saved["sources"].as_object();
        if sources.is_none_or(|s| {
            s.is_empty()
                || s.iter()
                    .any(|(id, key)| self.sources.get(id).is_none_or(|v| key.as_str() != Some(v)))
        }) {
            self.invalid += 1;
            return Ok(None);
        }
        let Some(id) = saved["plan_id"].as_str() else {
            return Ok(None);
        };
        let plan = store.read::<MixPlan>(&format!("plans/{id}.json"));
        match plan {
            Ok(mut p)
                if p.placements
                    .iter()
                    .all(|t| sources.unwrap().contains_key(&t.track_id)) =>
            {
                rebind(&mut p, policy)?;
                // Library-relative intensity changes when the pool expands.
                // Rescore a reused baseline from current profiles, never its
                // old per-passage memoized values.
                p.header.extensions.remove("set_composition");
                Ok(Some(p))
            }
            _ => {
                self.invalid += 1;
                Ok(None)
            }
        }
    }
    pub fn save_set(&self, store: &Store, policy: &MixingConfig, plan: &MixPlan) -> Result<()> {
        let sources: BTreeMap<_, _> = plan
            .placements
            .iter()
            .map(|p| (&p.track_id, &self.sources[&p.track_id]))
            .collect();
        self.write(
            store,
            "sets",
            &Self::set_key(policy)?,
            &json!({"plan_id":plan.plan_id,"sources":sources}),
        )
    }
}
pub(super) fn rebind(plan: &mut MixPlan, policy: &MixingConfig) -> Result<()> {
    plan.policy = policy.clone();
    plan.policy_hash = canonical_hash(policy)?;
    plan.header.extensions.remove("set_config");
    if let Some(config) = policy.header.extensions.get("set_config") {
        plan.header
            .extensions
            .insert("set_config".into(), config.clone());
    }
    plan.validate()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_baseline_survives_pool_growth_but_is_rescored_and_rejects_changed_sources() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(dir.path()).unwrap();
        let mut plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        plan.header.extensions.insert(
            "set_composition".into(),
            json!({"objective":9999,"passages":[{"cache_key":"old pool"}]}),
        );
        store
            .write(&format!("plans/{}.json", plan.plan_id), &plan)
            .unwrap();
        let mut cache = Cache::open(&plan.policy, &[]).unwrap();
        for p in &plan.placements {
            cache
                .sources
                .insert(p.track_id.clone(), "pinned generation".into());
        }
        cache.save_set(&store, &plan.policy, &plan).unwrap();
        cache
            .sources
            .insert("additional-song".into(), "new generation".into());
        let loaded = cache.saved_set(&store, &plan.policy).unwrap().unwrap();
        assert!(!loaded.header.extensions.contains_key("set_composition"));
        cache.sources.insert(
            plan.placements[0].track_id.clone(),
            "corrected generation".into(),
        );
        assert!(cache.saved_set(&store, &plan.policy).unwrap().is_none());
    }
    #[test]
    fn compatible_namespace_migration_preserves_negative_measurements() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(dir.path()).unwrap();
        let policy = MixingConfig::default();
        let mut legacy = Cache::open(&policy, &[]).unwrap();
        legacy.root = legacy.legacy_root.clone();
        legacy
            .write(&store, "windows", "rejected", &json!({"edges":[]}))
            .unwrap();
        let mut current = Cache::open(&policy, &[]).unwrap();
        current.migrate_legacy(&store).unwrap();
        assert!(current
            .read::<Value>(&store, "windows", "rejected")
            .unwrap()
            .is_some());
        assert!(!store.path(&legacy.root).unwrap().exists());
    }
    #[test]
    fn source_revisions_analysis_and_corrections_invalidate_pair_keys() {
        let track = Track {
            header: Header::default(),
            track_id: id(),
            audio_revision: 1,
            audio_hash: hash_bytes(b"source audio"),
            format: AudioFormat {
                sample_rate_hz: 48000,
                channels: 2,
                encoding: Encoding::IeeeFloat,
                container_bits: 32,
                valid_bits: 32,
                channel_mask: 3,
                frames: 480000,
            },
            source_locations: vec![],
            active_analysis: None,
            active_overrides: None,
            override_revision: 0,
            lifecycle: Lifecycle::Available,
        };
        let key = source_key(&track).unwrap();
        let mut edited = track.clone();
        edited.audio_revision += 1;
        assert_ne!(key, source_key(&edited).unwrap());
        edited = track.clone();
        edited.active_analysis = Some(FileRef {
            path: "analysis.json".into(),
            hash: hash_bytes(b"new analysis"),
        });
        assert_ne!(key, source_key(&edited).unwrap());
        edited = track.clone();
        edited.override_revision += 1;
        assert_ne!(key, source_key(&edited).unwrap());
        edited = track.clone();
        edited.audio_hash = hash_bytes(b"edited source audio");
        assert_ne!(key, source_key(&edited).unwrap());
    }
    #[test]
    fn equivalent_inline_and_recipe_defaults_share_the_completed_set() {
        let mut inline = MixingConfig::default();
        inline
            .header
            .extensions
            .insert("set_minutes".into(), json!(60));
        let mut recipe = inline.clone();
        recipe
            .header
            .extensions
            .insert("set_structure".into(), json!("story"));
        recipe
            .header
            .extensions
            .insert("required_spotify_pairs".into(), json!([]));
        recipe
            .header
            .extensions
            .insert("required_pair_mode".into(), json!("either"));
        assert_eq!(
            Cache::set_key(&inline).unwrap(),
            Cache::set_key(&recipe).unwrap()
        );
        recipe
            .header
            .extensions
            .insert("set_structure".into(), json!("wave"));
        assert_ne!(
            Cache::set_key(&inline).unwrap(),
            Cache::set_key(&recipe).unwrap()
        );
    }
    #[test]
    fn budgets_reuse_evidence_but_musical_changes_do_not() {
        let a = MixingConfig::default();
        let mut b = a.clone();
        b.header
            .extensions
            .insert("lazy_search_seconds".into(), json!(1));
        b.header
            .extensions
            .insert("set_config".into(), json!({"name":"another set"}));
        assert_eq!(policy_key(&a).unwrap(), policy_key(&b).unwrap());
        b.max_speed_deviation = 0.1;
        assert_ne!(policy_key(&a).unwrap(), policy_key(&b).unwrap());
        b = a.clone();
        b.header
            .extensions
            .insert("new_vocal_rule".into(), json!(true));
        assert_ne!(policy_key(&a).unwrap(), policy_key(&b).unwrap());
    }
    #[test]
    fn negative_decisions_survive_restart_and_corruption_becomes_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::init_legacy(dir.path()).unwrap();
        let policy = MixingConfig::default();
        let cache = Cache::open(&policy, &[]).unwrap();
        cache
            .write(
                &store,
                "windows",
                "rejected",
                &json!({"edges":[],"reason":"vocal overlap"}),
            )
            .unwrap();
        let mut restarted = Cache::open(&policy, &[]).unwrap();
        let value = restarted
            .read::<Value>(&store, "windows", "rejected")
            .unwrap()
            .unwrap();
        assert!(value["edges"].as_array().unwrap().is_empty());
        assert_eq!(restarted.hits, 1);
        std::fs::write(
            store
                .path(&format!("{}/windows/rejected.json", cache.root))
                .unwrap(),
            b"broken",
        )
        .unwrap();
        assert!(restarted
            .read::<Value>(&store, "windows", "rejected")
            .unwrap()
            .is_none());
        assert_eq!(restarted.invalid, 1);
    }
}
