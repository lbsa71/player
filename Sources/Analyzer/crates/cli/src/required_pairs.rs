//! Hard song-identity constraints, independent of cue/tactic quality.
use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

pub const KEY: &str = "required_spotify_pairs";
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Ordered,
    #[default]
    Either,
    Anywhere,
}
impl Mode {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "ordered" => Ok(Self::Ordered),
            "either" => Ok(Self::Either),
            "anywhere" => Ok(Self::Anywhere),
            _ => Err(Error::new(
                "REQUIRED_PAIR_MODE",
                "expected ordered, either or anywhere",
            )),
        }
    }
}
pub fn spotify_id(raw: &str) -> Result<String> {
    let value = raw.trim();
    let value = value
        .strip_prefix("spotify:track:")
        .or_else(|| value.strip_prefix("https://open.spotify.com/track/"))
        .unwrap_or(value);
    let value = value.split(['?', '#']).next().unwrap();
    require(
        value.len() == 22 && value.bytes().all(|b| b.is_ascii_alphanumeric()),
        "SPOTIFY_TRACK_ID",
        format!("expected a 22-character Spotify track ID, track URI or track URL: {raw}"),
    )?;
    Ok(value.into())
}
pub fn parse_pair(raw: &str) -> Result<[String; 2]> {
    let (a, b) = raw.split_once(',').ok_or_else(|| {
        Error::new(
            "REQUIRED_PAIR_FORMAT",
            "use --must-pair SPOTIFY_A,SPOTIFY_B; repeat the flag for more pairs",
        )
    })?;
    let pair = [spotify_id(a)?, spotify_id(b)?];
    require(
        pair[0] != pair[1],
        "REQUIRED_PAIR_CONFLICT",
        "a required pair must contain two different songs",
    )?;
    Ok(pair)
}
pub fn configure(policy: &mut MixingConfig, pairs: &[String], mode: Option<&str>) -> Result<()> {
    if pairs.is_empty() {
        return Ok(());
    }
    let mode = Mode::parse(mode.unwrap_or("either"))?;
    let pairs = pairs
        .iter()
        .map(|s| parse_pair(s))
        .collect::<Result<Vec<_>>>()?;
    policy.header.extensions.insert(KEY.into(), json!(pairs));
    policy
        .header
        .extensions
        .insert("required_pair_mode".into(), json!(mode));
    Ok(())
}
#[derive(Clone, Debug, Serialize)]
pub struct Pair {
    pub spotify_ids: [String; 2],
    pub track_ids: [String; 2],
    #[serde(skip)]
    pub indices: [usize; 2],
}
#[derive(Clone, Debug, Default)]
pub struct RequiredPairs {
    pub mode: Mode,
    pub pairs: Vec<Pair>,
    mask: u128,
}
impl RequiredPairs {
    pub fn resolve(
        store: &Store,
        policy: &MixingConfig,
        pool: &[Track],
        slots: usize,
    ) -> Result<Self> {
        let Some(value) = policy.header.extensions.get(KEY) else {
            return Ok(Self::default());
        };
        let raw: Vec<[String; 2]> = serde_json::from_value(value.clone())
            .map_err(|e| Error::new("REQUIRED_PAIR_FORMAT", e.to_string()))?;
        let mode = policy
            .header
            .extensions
            .get("required_pair_mode")
            .cloned()
            .map(serde_json::from_value)
            .transpose()?
            .unwrap_or_default();
        if raw.is_empty() {
            return Ok(Self {
                mode,
                ..Self::default()
            });
        }
        let mut identities = BTreeMap::<String, Vec<usize>>::new();
        for (i, t) in pool.iter().enumerate() {
            if let Some(s) = store.recording_metadata(t)?["trackID"].as_str() {
                if let Ok(id) = spotify_id(s) {
                    identities.entry(id).or_default().push(i);
                }
            }
        }
        let mut pairs = vec![];
        let mut seen = BTreeSet::new();
        for input in raw {
            let mut ids = [spotify_id(&input[0])?, spotify_id(&input[1])?];
            if mode != Mode::Ordered {
                ids.sort();
            }
            if !seen.insert(ids.clone()) {
                continue;
            }
            let mut indices = [0; 2];
            for i in 0..2 {
                let matches = identities.get(&ids[i]);
                require(matches.is_some_and(|v|v.len()==1),"REQUIRED_PAIR_TRACK",
                    format!("Spotify track {} must resolve to one available, fully analyzed song in this pool; found {}. Import/analyze it or resolve duplicate identities before searching.",ids[i],matches.map_or(0,Vec::len)))?;
                indices[i] = matches.unwrap()[0];
            }
            pairs.push(Pair {
                spotify_ids: ids,
                track_ids: indices.map(|i| pool[i].track_id.clone()),
                indices,
            });
        }
        Self::new(mode, pairs, slots)
    }
    pub(super) fn new(mode: Mode, pairs: Vec<Pair>, slots: usize) -> Result<Self> {
        let mut mask = 0u128;
        let mut outgoing = BTreeMap::<usize, BTreeSet<usize>>::new();
        let mut incoming = BTreeMap::<usize, BTreeSet<usize>>::new();
        for pair in &pairs {
            let [a, b] = pair.indices;
            require(
                a != b && a < 128 && b < 128,
                "REQUIRED_PAIR_CONFLICT",
                "pair identities must refer to distinct songs",
            )?;
            mask |= (1 << a) | (1 << b);
            outgoing.entry(a).or_default().insert(b);
            incoming.entry(b).or_default().insert(a);
            if mode == Mode::Either {
                outgoing.entry(b).or_default().insert(a);
                incoming.entry(a).or_default().insert(b);
            }
        }
        require(
            mask.count_ones() as usize <= slots,
            "REQUIRED_PAIR_CAPACITY",
            "required pairs contain more distinct songs than the requested set can contain",
        )?;
        if mode != Mode::Anywhere {
            let degree = if mode == Mode::Ordered { 1 } else { 2 };
            require(outgoing.values().chain(incoming.values()).all(|n|n.len()<=degree),
                "REQUIRED_PAIR_CONFLICT","required adjacencies branch: one linear set cannot satisfy these pairs without repeating a song")?;
            // Each component must be a path, never a cycle in a no-repeat set.
            fn cycle(
                node: usize,
                parent: Option<usize>,
                mode: Mode,
                edges: &BTreeMap<usize, BTreeSet<usize>>,
                active: &mut BTreeSet<usize>,
                done: &mut BTreeSet<usize>,
            ) -> bool {
                if active.contains(&node) {
                    return true;
                }
                if done.contains(&node) {
                    return false;
                }
                active.insert(node);
                for &next in edges.get(&node).into_iter().flatten() {
                    if mode == Mode::Either && Some(next) == parent {
                        continue;
                    }
                    if cycle(next, Some(node), mode, edges, active, done) {
                        return true;
                    }
                }
                active.remove(&node);
                done.insert(node);
                false
            }
            let mut done = BTreeSet::new();
            for &node in outgoing.keys() {
                require(
                    !cycle(node, None, mode, &outgoing, &mut BTreeSet::new(), &mut done),
                    "REQUIRED_PAIR_CONFLICT",
                    "required pairs form a cycle, which would require repeating a song",
                )?;
            }
        }
        Ok(Self { mode, pairs, mask })
    }
    fn neighbors(&self, node: usize) -> Vec<usize> {
        self.pairs
            .iter()
            .filter_map(|p| {
                let [a, b] = p.indices;
                if a == node {
                    Some(b)
                } else if b == node {
                    Some(a)
                } else {
                    None
                }
            })
            .collect()
    }
    pub fn start_allowed(&self, first: usize) -> bool {
        match self.mode {
            Mode::Ordered => !self.pairs.iter().any(|p| p.indices[1] == first),
            Mode::Either => self.neighbors(first).len() <= 1,
            Mode::Anywhere => true,
        }
    }
    #[cfg(feature = "cp-search")]
    pub(super) fn finish_allowed(&self, last: usize) -> bool {
        match self.mode {
            Mode::Ordered => !self.pairs.iter().any(|p| p.indices[0] == last),
            Mode::Either => self.neighbors(last).len() <= 1,
            Mode::Anywhere => true,
        }
    }

    pub fn permits_edge(&self, from: usize, to: usize) -> bool {
        match self.mode {
            Mode::Ordered => self.pairs.iter().all(|p| {
                (p.indices[0] != from || p.indices[1] == to)
                    && (p.indices[1] != to || p.indices[0] == from)
            }),
            Mode::Either => [from, to].into_iter().all(|node| {
                let neighbors = self.neighbors(node);
                neighbors.len() < 2 || neighbors.contains(&(if node == from { to } else { from }))
            }),
            Mode::Anywhere => true,
        }
    }
    pub fn allows(&self, used: u128, last: usize, to: usize, remaining: usize) -> bool {
        if used & (1 << to) != 0 || !self.permits_edge(last, to) {
            return false;
        }
        let after = used | (1 << to);
        if (self.mask & !after).count_ones() as usize > remaining {
            return false;
        }
        if self.mode == Mode::Either {
            let pending = self
                .neighbors(last)
                .into_iter()
                .filter(|n| used & (1 << n) == 0)
                .collect::<Vec<_>>();
            if !pending.is_empty() && !pending.contains(&to) {
                return false;
            }
            let next = self.neighbors(to);
            if next.iter().any(|&n| used & (1 << n) != 0 && n != last)
                || next.iter().filter(|&&n| after & (1 << n) == 0).count() > 1
            {
                return false;
            }
        }
        true
    }
    pub fn progress(&self, used: u128) -> u128 {
        used & self.mask
    }
    pub fn complete_used(&self, used: u128) -> bool {
        used & self.mask == self.mask
    }
    #[cfg(feature = "cp-search")]
    pub(super) fn required_runway(&self) -> mixlib_set_search::coverage::RequiredRunway {
        let mut internal = 0;
        for track in 0..128 {
            let bit = 1_u128 << track;
            if self.mask & bit != 0 && !self.start_allowed(track) && !self.finish_allowed(track) {
                internal |= bit;
            }
        }
        mixlib_set_search::coverage::RequiredRunway {
            members: self.mask,
            internal,
        }
    }

    /// A connected component of direct requirements is a contiguous block
    /// in any unique-song route. Every edge must survive in one orientation.
    #[cfg(feature = "cp-search")]
    pub(super) fn blocked_chain(
        &self,
        permits: impl Fn(usize, usize) -> bool,
    ) -> Option<Vec<usize>> {
        self.forced_chain_edges(permits).err()
    }

    /// Orient every either-order chain that has only one surviving direction.
    /// An edge in the result fixes its source's successor and its target's
    /// predecessor. An error carries a chain with neither orientation left.
    #[cfg(feature = "cp-search")]
    pub(super) fn forced_chain_edges(
        &self,
        permits: impl Fn(usize, usize) -> bool,
    ) -> std::result::Result<Vec<(usize, usize)>, Vec<usize>> {
        match self.mode {
            Mode::Anywhere => Ok(vec![]),
            Mode::Ordered => {
                for pair in &self.pairs {
                    let [from, to] = pair.indices;
                    if !permits(from, to) {
                        return Err(vec![from, to]);
                    }
                }
                Ok(vec![])
            }
            Mode::Either => {
                let mut forced = Vec::new();
                let mut remaining = self.mask;
                while remaining != 0 {
                    let first = (0..128)
                        .find(|&i| remaining & (1_u128 << i) != 0 && self.neighbors(i).len() == 1)
                        .expect("validated required chain has an endpoint");
                    let mut chain = vec![first];
                    let mut next = first;
                    let mut previous = None;
                    loop {
                        remaining &= !(1_u128 << next);
                        let Some(neighbor) = self
                            .neighbors(next)
                            .into_iter()
                            .find(|&n| Some(n) != previous)
                        else {
                            break;
                        };
                        chain.push(neighbor);
                        previous = Some(next);
                        next = neighbor;
                    }
                    let forward = chain.windows(2).all(|w| permits(w[0], w[1]));
                    let reverse = chain.windows(2).all(|w| permits(w[1], w[0]));
                    match (forward, reverse) {
                        (false, false) => return Err(chain),
                        (true, true) => {}
                        (true, false) => forced.extend(chain.windows(2).map(|w| (w[0], w[1]))),
                        (false, true) => {
                            forced.extend(chain.windows(2).rev().map(|w| (w[1], w[0])))
                        }
                    }
                }
                Ok(forced)
            }
        }
    }
    pub fn satisfied(&self, plan: &MixPlan) -> bool {
        self.pairs.iter().all(|p| match self.mode {
            Mode::Anywhere => p
                .track_ids
                .iter()
                .all(|id| plan.placements.iter().any(|t| &t.track_id == id)),
            mode => plan.placements.windows(2).any(|w| {
                (w[0].track_id == p.track_ids[0] && w[1].track_id == p.track_ids[1])
                    || (mode == Mode::Either
                        && w[0].track_id == p.track_ids[1]
                        && w[1].track_id == p.track_ids[0])
            }),
        })
    }
    pub fn priority(&self, from: usize, to: usize) -> f64 {
        if self.mode != Mode::Anywhere
            && self.pairs.iter().any(|p| {
                p.indices == [from, to] || (self.mode == Mode::Either && p.indices == [to, from])
            })
        {
            32.
        } else if self.mask & ((1 << from) | (1 << to)) != 0 {
            4.
        } else {
            0.
        }
    }
    pub fn evidence(&self) -> Value {
        json!({"mode":self.mode,"pairs":self.pairs,"hard_constraint":true})
    }
    pub fn annotate(&self, plan: &mut MixPlan) -> Result<()> {
        require(
            self.satisfied(plan),
            "REQUIRED_PAIRS_UNSATISFIED",
            "complete set does not satisfy all required Spotify pairs",
        )?;
        if !self.pairs.is_empty() {
            let mut evidence = self.evidence();
            evidence["satisfied"] = json!(true);
            plan.header
                .extensions
                .insert("required_pairs".into(), evidence);
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../tests/support.rs"]
pub(super) mod test_support;
#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "cp-search")]
    #[test]
    fn required_chain_proof_matches_all_orientations_of_three_songs() {
        let pairs = [[0, 1], [1, 2]]
            .map(|indices| Pair {
                spotify_ids: indices.map(|i| format!("spotify-{i}")),
                track_ids: indices.map(|i| format!("track-{i}")),
                indices,
            })
            .to_vec();
        let orders = [[0, 1, 2], [2, 1, 0]];
        for mode in [Mode::Ordered, Mode::Either, Mode::Anywhere] {
            let required = RequiredPairs::new(mode, pairs.clone(), 3).unwrap();
            for edges in 0..16 {
                let permits = |from, to| {
                    [(0, 1), (1, 0), (1, 2), (2, 1)]
                        .iter()
                        .position(|&edge| edge == (from, to))
                        .is_some_and(|bit| edges & (1 << bit) != 0)
                };
                let feasible = match mode {
                    Mode::Anywhere => true,
                    Mode::Ordered => permits(0, 1) && permits(1, 2),
                    Mode::Either => orders
                        .iter()
                        .any(|order| order.windows(2).all(|w| permits(w[0], w[1]))),
                };
                let result = required.forced_chain_edges(permits);
                assert_eq!(result.is_ok(), feasible);
                if mode == Mode::Either && feasible {
                    let surviving = orders
                        .iter()
                        .filter(|order| order.windows(2).all(|w| permits(w[0], w[1])))
                        .collect::<Vec<_>>();
                    let expected = if let [order] = surviving.as_slice() {
                        order.windows(2).map(|w| (w[0], w[1])).collect::<Vec<_>>()
                    } else {
                        vec![]
                    };
                    assert_eq!(result.unwrap(), expected);
                }
            }
        }
    }

    #[test]
    fn resolves_spotify_metadata_only_within_the_prepared_pool() {
        let dir = tempfile::tempdir().unwrap();
        let music = dir.path().join("music");
        std::fs::create_dir(&music).unwrap();
        test_support::tone(&music.join("one.wav"), 0.1, 440.);
        test_support::tone(&music.join("two.wav"), 0.1, 550.);
        let store = Store::init(&dir.path().join("library")).unwrap();
        let mut config: AnalysisConfig = store.read("config/analysis.json").unwrap();
        config.settling_ms = 0;
        store.write("config/analysis.json", &config).unwrap();
        scan(&store, &[music], false, &Cancellation::default()).unwrap();
        let tracks = store.tracks().unwrap();
        assert_eq!(tracks.len(), 2);
        let ids = ["4xv5HodnhPQfqEj7GUCdI1", "0suNzFaLZs12YO4hPQ7JiB"];
        for (track, id) in tracks.iter().zip(ids) {
            let path = store.source(track).unwrap();
            let path = std::path::PathBuf::from(format!("{}.json", path.display()));
            let mut document: Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            document["trackID"] = json!(format!("spotify:track:{id}"));
            std::fs::write(path, serde_json::to_vec(&document).unwrap()).unwrap();
        }
        let mut policy = MixingConfig::default();
        configure(
            &mut policy,
            &[
                format!("{},{}", ids[0], ids[1]),
                format!("spotify:track:{},{}", ids[0], ids[1]),
            ],
            None,
        )
        .unwrap();
        let required = RequiredPairs::resolve(&store, &policy, &tracks, 3).unwrap();
        assert_eq!(required.pairs.len(), 1, "deduplicate normalized requests");
        assert_eq!(required.mode, Mode::Either);
        assert_eq!(
            required.pairs[0].track_ids,
            [tracks[1].track_id.clone(), tracks[0].track_id.clone()]
        );
        assert_eq!(
            RequiredPairs::resolve(&store, &policy, &tracks[..1], 3)
                .unwrap_err()
                .code,
            "REQUIRED_PAIR_TRACK"
        );
    }
    #[test]
    fn repeatable_cli_pairs_and_modes_are_available_for_full_and_short_sets() {
        use clap::Parser;
        let pair = "4xv5HodnhPQfqEj7GUCdI1,0suNzFaLZs12YO4hPQ7JiB";
        let parsed = Args::try_parse_from([
            "mixlib",
            "search-set",
            "--tracks",
            "8",
            "--must-pair",
            pair,
            "--must-pair",
            pair,
            "--must-pair-mode",
            "either",
        ])
        .unwrap();
        let Command::SearchSet {
            must_pair,
            must_pair_mode,
            ..
        } = parsed.command
        else {
            panic!()
        };
        assert_eq!(must_pair.len(), 2);
        assert_eq!(must_pair_mode.as_deref(), Some("either"));
        assert!(Args::try_parse_from([
            "mixlib",
            "search-set",
            "--set-minutes",
            "60",
            "--must-pair",
            pair
        ])
        .is_ok());
        assert!(
            Args::try_parse_from(["mixlib", "search-set", "--must-pair-mode", "either"]).is_err()
        );
    }
    fn requirement(mode: Mode, edges: &[[usize; 2]]) -> Result<RequiredPairs> {
        RequiredPairs::new(
            mode,
            edges
                .iter()
                .map(|&indices| Pair {
                    spotify_ids: indices.map(|i| format!("{i:022}")),
                    track_ids: indices.map(|i| format!("track-{i}")),
                    indices,
                })
                .collect(),
            6,
        )
    }
    #[test]
    fn spotify_track_inputs_are_normalized_and_malformed_pairs_fail() {
        let a = "4xv5HodnhPQfqEj7GUCdI1";
        assert_eq!(spotify_id(a).unwrap(), a);
        assert_eq!(spotify_id(&format!("spotify:track:{a}")).unwrap(), a);
        assert_eq!(
            spotify_id(&format!("https://open.spotify.com/track/{a}?si=abc")).unwrap(),
            a
        );
        assert!(spotify_id("https://open.spotify.com/album/4xv5HodnhPQfqEj7GUCdI1").is_err());
        assert!(parse_pair(a).is_err());
        assert!(parse_pair(&format!("{a},{a}")).is_err());
        assert!(parse_pair(&format!("{a},not-a-track-id")).is_err());
    }
    #[test]
    fn impossible_branches_cycles_and_excess_song_counts_fail_early() {
        assert!(requirement(Mode::Ordered, &[[0, 1], [0, 2]]).is_err());
        assert!(requirement(Mode::Ordered, &[[0, 2], [1, 2]]).is_err());
        assert!(requirement(Mode::Ordered, &[[0, 1], [1, 2], [2, 0]]).is_err());
        assert!(requirement(Mode::Either, &[[0, 1], [0, 2], [0, 3]]).is_err());
        assert!(requirement(Mode::Either, &[[0, 1], [1, 2], [2, 0]]).is_err());
        assert!(requirement(Mode::Anywhere, &[[0, 1], [1, 2], [2, 0]]).is_ok());
        let r = requirement(Mode::Ordered, &[[0, 1], [1, 2], [3, 4]]).unwrap();
        assert!(RequiredPairs::new(r.mode, r.pairs, 4).is_err());
    }

    #[cfg(feature = "cp-search")]
    #[test]
    fn only_required_songs_forbidden_at_both_bookends_reserve_runway() {
        for (mode, internal) in [
            (Mode::Ordered, 0b10),
            (Mode::Either, 0b10),
            (Mode::Anywhere, 0),
        ] {
            let runway = requirement(mode, &[[0, 1], [1, 2], [3, 4]])
                .unwrap()
                .required_runway();
            assert_eq!(runway.members, 0b11111);
            assert_eq!(runway.internal, internal);
        }
        let pair = requirement(Mode::Either, &[[0, 1]])
            .unwrap()
            .required_runway();
        assert_eq!(pair.members, 0b11);
        assert_eq!(pair.internal, 0);
    }
    #[test]
    fn prefix_pruning_matches_exhaustive_pair_semantics() {
        fn permutations(values: &mut [usize], at: usize, out: &mut Vec<Vec<usize>>) {
            if at == values.len() {
                out.push(values.to_vec());
                return;
            }
            for i in at..values.len() {
                values.swap(at, i);
                permutations(values, at + 1, out);
                values.swap(at, i);
            }
        }
        let mut orders = vec![];
        permutations(&mut [0, 1, 2, 3, 4, 5], 0, &mut orders);
        for mode in [Mode::Ordered, Mode::Either, Mode::Anywhere] {
            let r = requirement(mode, &[[0, 1], [1, 2], [3, 4]]).unwrap();
            for order in &orders {
                let expected = r.pairs.iter().all(|p| match mode {
                    Mode::Anywhere => p.indices.iter().all(|n| order.contains(n)),
                    Mode::Ordered => order.windows(2).any(|w| w == p.indices),
                    Mode::Either => order
                        .windows(2)
                        .any(|w| w == p.indices || w == [p.indices[1], p.indices[0]]),
                });
                #[cfg(feature = "cp-search")]
                if expected {
                    assert!(r.start_allowed(order[0]));
                    assert!(r.finish_allowed(*order.last().unwrap()));
                    assert!(order.windows(2).all(|w| r.permits_edge(w[0], w[1])));
                }
                let mut used = 1 << order[0];
                let mut admitted = r.start_allowed(order[0]);
                for (i, w) in order.windows(2).enumerate() {
                    admitted &= r.allows(used, w[0], w[1], order.len() - i - 2);
                    used |= 1 << w[1];
                }
                admitted &= r.complete_used(used);
                assert_eq!(admitted, expected, "{mode:?} {order:?}");
            }
            assert!(!r.complete_used((1 << 0) | (1 << 1) | (1 << 2) | (1 << 3) | (1 << 5)));
            assert!(
                !r.allows(1 << 5, 5, 0, 2),
                "remaining slots cannot hide missing required songs"
            );
        }
    }
    #[test]
    fn final_validation_checks_actual_adjacency_not_just_membership() {
        let r = requirement(Mode::Ordered, &[[0, 1], [1, 2]]).unwrap();
        let mut plan: MixPlan =
            serde_json::from_str(include_str!("../../../examples/mix-plan.json")).unwrap();
        let p = plan.placements[0].clone();
        plan.placements = (0..3)
            .map(|i| {
                let mut p = p.clone();
                p.track_id = format!("track-{i}");
                p
            })
            .collect();
        r.annotate(&mut plan).unwrap();
        assert_eq!(plan.header.extensions["required_pairs"]["satisfied"], true);
        plan.placements.swap(0, 1);
        assert!(r.annotate(&mut plan).is_err());
    }
}
