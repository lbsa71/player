//! One portable, versioned recipe per set. Applying it never writes library defaults.
use super::*;
use crate::required_pairs::{self, Mode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub(super) const ENGINE_KEY: &str = "duration_search_engine";
pub(super) const CACHE_MODE_KEY: &str = "duration_search_cache_mode";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheMode {
    Cold,
    WarmFacts,
    #[default]
    Resume,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Legacy,
    Cp,
}

fn auto() -> String {
    "auto".into()
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<Engine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_mode: Option<CacheMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measurements: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SetConfig {
    pub version: u32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_minutes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracks: Option<u8>,
    #[serde(default = "auto")]
    pub structure: String,
    #[serde(default)]
    pub must_pairs: Vec<[String; 2]>,
    #[serde(default)]
    pub pair_mode: Mode,
    #[serde(default)]
    pub search: SearchBudget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incumbent: Option<String>,
}
impl SetConfig {
    pub fn starter(
        name: String,
        minutes: Option<u64>,
        tracks: Option<u8>,
        structure: String,
    ) -> Result<Self> {
        let mut config = Self {
            version: 1,
            name,
            duration_minutes: minutes,
            tracks,
            structure,
            must_pairs: vec![],
            pair_mode: Mode::Either,
            search: SearchBudget::default(),
            incumbent: None,
        };
        config.normalize()?;
        // Make the generated recipe's defaults explicit and easy to edit.
        let (measurements, seconds) = config.budgets();
        config.search = SearchBudget {
            engine: None,
            cache_mode: None,
            measurements: Some(measurements),
            seconds: Some(seconds),
        };
        Ok(config)
    }
    pub fn normalize(&mut self) -> Result<()> {
        require(
            matches!(self.version, 1 | 2),
            "SET_CONFIG_VERSION",
            "unsupported set config version; expected 1 or 2",
        )?;
        self.name = self.name.trim().to_owned();
        require(
            !self.name.is_empty(),
            "SET_CONFIG_NAME",
            "set name must not be empty",
        )?;
        require(
            self.duration_minutes.is_none() || (self.tracks.is_none() && self.incumbent.is_none()),
            "SET_CONFIG_TARGET",
            "duration_minutes cannot be combined with tracks or incumbent",
        )?;
        if self.duration_minutes.is_none() && self.tracks.is_none() && self.incumbent.is_none() {
            self.duration_minutes = Some(60);
        }
        require(
            (self.search.engine.is_none() && self.search.cache_mode.is_none())
                || (self.version == 2 && self.duration_minutes.is_some()),
            "SET_CONFIG_ENGINE",
            "search.engine and search.cache_mode require a version-2 duration set",
        )?;
        if let Some(minutes) = self.duration_minutes {
            crate::composition::Request::new(minutes, &self.structure)?;
        } else {
            self.tracks = Some(self.tracks.unwrap_or(8));
            require(
                self.tracks.is_none_or(|n| (3..=8).contains(&n)),
                "SET_CONFIG_TRACKS",
                "short set track count must be 3–8",
            )?;
            require(
                self.structure == "auto",
                "SET_CONFIG_STRUCTURE",
                "structure applies to duration sets; short sets use auto",
            )?;
        }
        require(
            self.incumbent.as_ref().is_none_or(|v| !v.trim().is_empty()),
            "SET_CONFIG_INCUMBENT",
            "incumbent must not be empty",
        )?;
        require(
            self.search
                .measurements
                .is_none_or(|n| (1..=100000).contains(&n))
                && self.search.seconds.is_none_or(|n| (1..=86400).contains(&n)),
            "SET_CONFIG_BUDGET",
            "measurements must be 1–100000 and seconds 1–86400",
        )?;
        for pair in &mut self.must_pairs {
            *pair = [
                required_pairs::spotify_id(&pair[0])?,
                required_pairs::spotify_id(&pair[1])?,
            ];
            require(
                pair[0] != pair[1],
                "REQUIRED_PAIR_CONFLICT",
                "a required pair must contain two different songs",
            )?;
            if self.pair_mode != Mode::Ordered {
                pair.sort();
            }
        }
        self.must_pairs.sort();
        self.must_pairs.dedup();
        Ok(())
    }
    fn budgets(&self) -> (u64, u64) {
        let defaults = match self.duration_minutes {
            Some(120) => (16384, 14400),
            Some(_) => (8192, 7200),
            None => (1536, 1200),
        };
        (
            self.search.measurements.unwrap_or(defaults.0),
            self.search.seconds.unwrap_or(defaults.1),
        )
    }
    pub fn apply(&self, policy: &mut MixingConfig, source: &Path) -> Result<usize> {
        // A recipe owns these settings, including an explicitly empty pair list.
        // Never inherit another set's target, incumbent or required songs.
        for key in [
            "set_minutes",
            "set_structure",
            "lazy_incumbent_plan",
            required_pairs::KEY,
            "required_pair_mode",
            "set_config",
            ENGINE_KEY,
            CACHE_MODE_KEY,
        ] {
            policy.header.extensions.remove(key);
        }
        if let Some(engine) = self.search.engine {
            policy
                .header
                .extensions
                .insert(ENGINE_KEY.into(), json!(engine));
        }
        if let Some(mode) = self.search.cache_mode {
            policy
                .header
                .extensions
                .insert(CACHE_MODE_KEY.into(), json!(mode));
        }
        let (measurements, seconds) = self.budgets();
        policy
            .header
            .extensions
            .insert("lazy_measurement_budget".into(), json!(measurements));
        policy
            .header
            .extensions
            .insert("lazy_search_seconds".into(), json!(seconds));
        if let Some(minutes) = self.duration_minutes {
            policy
                .header
                .extensions
                .insert("set_minutes".into(), json!(minutes));
            policy
                .header
                .extensions
                .insert("set_structure".into(), json!(self.structure));
        }
        if let Some(id) = &self.incumbent {
            policy
                .header
                .extensions
                .insert("lazy_incumbent_plan".into(), json!(id));
        }
        policy
            .header
            .extensions
            .insert(required_pairs::KEY.into(), json!(self.must_pairs));
        policy
            .header
            .extensions
            .insert("required_pair_mode".into(), json!(self.pair_mode));
        let mut effective = self.clone();
        effective.search = SearchBudget {
            engine: self.search.engine,
            cache_mode: self.search.cache_mode,
            measurements: Some(measurements),
            seconds: Some(seconds),
        };
        policy.header.extensions.insert(
            "set_config".into(),
            json!({"name":self.name,"path":source,
            "hash":canonical_hash(&effective)?,"effective":effective}),
        );
        Ok(usize::from(self.tracks.unwrap_or(8)))
    }
}
pub fn load(path: &Path) -> Result<(SetConfig, PathBuf)> {
    let source = std::fs::canonicalize(path)?;
    require(
        std::fs::metadata(&source)?.len() <= MAX_JSON,
        "SET_CONFIG_SIZE",
        "set config exceeds JSON size limit",
    )?;
    let mut document: SetConfig = serde_json::from_slice(&std::fs::read(&source)?)
        .map_err(|e| Error::new("SET_CONFIG_JSON", e.to_string()))?;
    document.normalize()?;
    Ok((document, source))
}
pub fn create(path: &Path, document: &SetConfig) -> Result<Value> {
    use std::io::Write;
    require(
        path.extension().is_some_and(|s| s == "json"),
        "SET_CONFIG_PATH",
        "use a .json path for the set config",
    )?;
    require(
        !path.exists(),
        "OUTPUT_EXISTS",
        "set config already exists; edit it or choose another path",
    )?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(&serde_json::to_vec_pretty(document)?)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist_noclobber(path)
        .map_err(|e| Error::new("SET_CONFIG_WRITE", e.to_string()))?;
    Ok(json!({"config":std::fs::canonicalize(path)?,"settings":document}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn decode(value: Value) -> Result<SetConfig> {
        let mut c: SetConfig = serde_json::from_value(value)
            .map_err(|e| Error::new("SET_CONFIG_JSON", e.to_string()))?;
        c.normalize()?;
        Ok(c)
    }
    #[test]
    fn recipes_resolve_hour_two_hour_and_short_defaults() {
        let hour = decode(json!({"version":1,"name":"Set"})).unwrap();
        assert_eq!(hour.duration_minutes, Some(60));
        assert_eq!(hour.pair_mode, Mode::Either);
        assert_eq!(hour.budgets(), (8192, 7200));
        let two = decode(json!({"version":1,"name":"Set","duration_minutes":120})).unwrap();
        assert_eq!(two.budgets(), (16384, 14400));
        let short = decode(json!({"version":1,"name":"Set","tracks":8})).unwrap();
        assert_eq!(short.duration_minutes, None);
        assert_eq!(short.budgets(), (1536, 1200));
    }
    #[test]
    fn invalid_or_misspelled_config_never_silently_changes_the_request() {
        for input in [
            json!({"version":3,"name":"Set"}),
            json!({"version":1,"name":"Set","search":{"engine":"cp"}}),
            json!({"version":2,"name":"Set","tracks":8,"search":{"engine":"cp"}}),
            json!({"version":1,"name":" "}),
            json!({"version":1,"name":"Set","duration_minutes":90}),
            json!({"version":1,"name":"Set","duration_minutes":60,"tracks":8}),
            json!({"version":1,"name":"Set","duration_minutes":60,"incumbent":"plan"}),
            json!({"version":1,"name":"Set","tracks":2}),
            json!({"version":1,"name":"Set","tracks":8,"structure":"story"}),
            json!({"version":1,"name":"Set","search":{"seconds":0}}),
            json!({"version":1,"name":"Set","search":{"measurements":100001}}),
            json!({"version":1,"name":"Set","must_pair":[]}),
            json!({"version":1,"name":"Set","search":{"second":60}}),
            json!({"version":1,"name":"Set","must_pairs":[["bad","ids"]]}),
        ] {
            assert!(decode(input.clone()).is_err(), "{input}");
        }
    }
    #[test]
    fn a_version_two_recipe_can_select_cp_without_changing_legacy_defaults() {
        let cp = decode(json!({"version":2,"name":"CP","search":{"engine":"cp"}})).unwrap();
        let mut policy = MixingConfig::default();
        cp.apply(&mut policy, Path::new("cp.json")).unwrap();
        assert_eq!(policy.header.extensions[ENGINE_KEY], "cp");
        let legacy = decode(json!({"version":1,"name":"Legacy"})).unwrap();
        legacy.apply(&mut policy, Path::new("legacy.json")).unwrap();
        assert!(!policy.header.extensions.contains_key(ENGINE_KEY));
    }

    #[test]
    fn each_recipe_owns_target_pairs_incumbent_and_budget_without_mutating_defaults() {
        let mut defaults = MixingConfig::default();
        defaults
            .header
            .extensions
            .insert("set_minutes".into(), json!(120));
        defaults
            .header
            .extensions
            .insert("lazy_incumbent_plan".into(), json!("other-set"));
        defaults
            .header
            .extensions
            .insert(required_pairs::KEY.into(), json!([["other", "songs"]]));
        let before = canonical_hash(&defaults).unwrap();
        let c=decode(json!({"version":1,"name":"Independent","tracks":8,"must_pairs":[],"search":{"seconds":300}})).unwrap();
        let mut policy = defaults.clone();
        assert_eq!(c.apply(&mut policy, Path::new("set.json")).unwrap(), 8);
        assert!(!policy.header.extensions.contains_key("set_minutes"));
        assert!(!policy.header.extensions.contains_key("lazy_incumbent_plan"));
        assert_eq!(policy.header.extensions[required_pairs::KEY], json!([]));
        assert_eq!(policy.header.extensions["lazy_search_seconds"], 300);
        assert_eq!(
            policy.header.extensions["set_config"]["effective"]["search"]["measurements"],
            1536
        );
        assert_eq!(canonical_hash(&defaults).unwrap(), before);
    }
    #[test]
    fn creation_is_independent_of_a_library_and_preserves_existing_recipes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sets/my-set.json");
        let args = Args::try_parse_from([
            "mixlib",
            "--library",
            dir.path().join("missing-library").to_str().unwrap(),
            "init-set",
            "--output",
            path.to_str().unwrap(),
            "--name",
            "Evening",
            "--set-minutes",
            "120",
        ])
        .unwrap();
        run(&args, &Cancellation::default()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let (loaded, canonical) = load(&path).unwrap();
        assert_eq!(loaded.name, "Evening");
        assert_eq!(loaded.duration_minutes, Some(120));
        assert!(canonical.is_absolute());
        assert_eq!(
            run(&args, &Cancellation::default()).unwrap_err().code,
            "OUTPUT_EXISTS"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!dir.path().join("missing-library").exists());
    }
    #[test]
    fn cli_config_cannot_be_mixed_with_inline_set_choices() {
        assert!(Args::try_parse_from(["mixlib", "search-set", "--config", "set.json"]).is_ok());
        for (flag, value) in [
            ("--structure", "story"),
            ("--tracks", "8"),
            ("--seconds", "10"),
            ("--measurements", "100"),
            ("--set-minutes", "60"),
            ("--incumbent", "plan"),
            ("--must-pair", "A,B"),
        ] {
            assert!(
                Args::try_parse_from(["mixlib", "search-set", "--config", "set.json", flag, value])
                    .is_err(),
                "{flag}"
            );
        }
    }
    #[test]
    fn pair_normalization_and_effective_hash_are_reproducible() {
        let a = "4xv5HodnhPQfqEj7GUCdI1";
        let b = "0suNzFaLZs12YO4hPQ7JiB";
        let c = decode(
            json!({"version":1,"name":"Set","must_pairs":[[a,b],[format!("spotify:track:{b}"),a]]}),
        )
        .unwrap();
        assert_eq!(c.must_pairs.len(), 1);
        let mut p = MixingConfig::default();
        c.apply(&mut p, Path::new("one.json")).unwrap();
        let hash = p.header.extensions["set_config"]["hash"].clone();
        c.apply(&mut p, Path::new("another.json")).unwrap();
        assert_eq!(
            hash, p.header.extensions["set_config"]["hash"],
            "moving a recipe does not change its content hash"
        );
    }
}
