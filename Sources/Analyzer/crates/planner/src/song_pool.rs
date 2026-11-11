//! Identity policy for automatic song pools. Capture hashes identify recordings;
//! recorder track IDs identify songs across separate captures of the same audio.
use mixlib_catalog::Store;
use mixlib_types::*;
use std::collections::BTreeSet;

struct Identity {
    track_id: String,
    audio_hash: String,
    song_id: Option<String>,
    prepared: bool,
    recorded_at: String,
    source_start_seconds: Option<f64>,
    duration_seconds: f64,
}
fn unique_indices(identities: &[Identity]) -> Vec<usize> {
    let mut order: Vec<_> = (0..identities.len()).collect();
    // A materially fuller capture of the same source song preserves more cue
    // freedom. Remove only interval-dominated captures, never infer identity or
    // missing source offsets from titles. Then prefer already prepared audio.
    order.retain(|&i| {
        !identities.iter().enumerate().any(|(j, other)| {
            let current = &identities[i];
            if i == j || current.song_id.is_none() || current.song_id != other.song_id {
                return false;
            }
            match (current.source_start_seconds, other.source_start_seconds) {
                (Some(a), Some(b)) => {
                    b <= a + 0.5
                        && b + other.duration_seconds >= a + current.duration_seconds - 0.5
                        && other.duration_seconds
                            >= current.duration_seconds + (current.duration_seconds * 0.05).max(15.)
                }
                _ => false,
            }
        })
    });
    // Preserve an existing deep analysis among captures with equivalent coverage.
    // Among prepared captures, retain the earliest known recording consistently.
    order.sort_by(|&a, &b| {
        let (a, b) = (&identities[a], &identities[b]);
        b.prepared
            .cmp(&a.prepared)
            .then(a.recorded_at.cmp(&b.recorded_at))
            .then(a.track_id.cmp(&b.track_id))
    });
    let mut audio = BTreeSet::new();
    let mut songs = BTreeSet::new();
    order.retain(|&index| {
        let identity = &identities[index];
        if audio.contains(&identity.audio_hash)
            || identity
                .song_id
                .as_ref()
                .is_some_and(|id| songs.contains(id))
        {
            return false;
        }
        audio.insert(identity.audio_hash.clone());
        if let Some(id) = &identity.song_id {
            songs.insert(id.clone());
        }
        true
    });
    order
}

/// Titles are deliberately not identity: remixes and different releases remain
/// separate unless the recorder supplies the same nonempty source track ID.
pub fn unique_song_pool(store: &Store, tracks: Vec<Track>) -> Result<Vec<Track>> {
    let identities = tracks
        .iter()
        .map(|track| {
            let metadata = store.recording_metadata(track)?;
            Ok(Identity {
                track_id: track.track_id.clone(),
                audio_hash: track.audio_hash.digest.clone(),
                song_id: metadata
                    .get("trackID")
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(|id| id.strip_prefix("spotify:track:").unwrap_or(id).to_owned()),
                prepared: track.active_analysis.is_some()
                    && store
                        .analysis(track)?
                        .artifacts
                        .contains_key("tactic_analysis"),
                source_start_seconds: metadata
                    .get("observedStartPositionSeconds")
                    .and_then(|v| v.as_f64())
                    .filter(|v| v.is_finite() && *v >= 0.),
                duration_seconds: track.format.frames as f64
                    / f64::from(track.format.sample_rate_hz),
                recorded_at: metadata
                    .get("startedAt")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_owned(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let mut result: Vec<_> = unique_indices(&identities)
        .into_iter()
        .map(|i| tracks[i].clone())
        .collect();
    result.sort_by(|a, b| a.track_id.cmp(&b.track_id));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn capture(id: &str, song: Option<&str>, hash: &str, prepared: bool, date: &str) -> Identity {
        Identity {
            track_id: id.into(),
            song_id: song.map(str::to_owned),
            audio_hash: hash.into(),
            prepared,
            recorded_at: date.into(),
            source_start_seconds: None,
            duration_seconds: 300.,
        }
    }
    #[test]
    fn fuller_capture_wins_without_mistaking_a_later_excerpt_for_complete_audio() {
        let mut old = capture("old", Some("song"), "a", true, "1");
        old.source_start_seconds = Some(0.);
        old.duration_seconds = 220.;
        let mut full = capture("full", Some("song"), "b", false, "2");
        full.source_start_seconds = Some(0.);
        full.duration_seconds = 297.;
        let mut tail = capture("tail", Some("song"), "c", false, "1.5");
        tail.source_start_seconds = Some(222.);
        tail.duration_seconds = 75.2;
        assert_eq!(unique_indices(&[old, full, tail]), vec![1]);
        let mut old = capture("old", Some("song"), "a", true, "1");
        old.source_start_seconds = Some(0.);
        let mut later = capture("later", Some("song"), "b", false, "2");
        later.source_start_seconds = Some(100.);
        later.duration_seconds = 350.;
        assert_eq!(unique_indices(&[old, later]), vec![0]);
    }
    #[test]
    fn repeated_capture_is_not_a_new_song_and_prepared_copy_wins() {
        let identities = vec![
            capture(
                "new",
                Some("same-song"),
                "different-pcm",
                false,
                "2026-11-11",
            ),
            capture("old", Some("same-song"), "original-pcm", true, "2026-11-06"),
            capture(
                "remix",
                Some("different-song"),
                "remix-pcm",
                false,
                "2026-11-11",
            ),
        ];
        assert_eq!(unique_indices(&identities), vec![1, 2]);
    }
    #[test]
    fn prepared_captures_are_stable_and_missing_ids_do_not_collapse_songs() {
        let identities = vec![
            capture("new", Some("same"), "b", true, "2026-11-11"),
            capture("old", Some("same"), "a", true, "2026-11-06"),
            capture("unknown1", None, "c", false, ""),
            capture("unknown2", None, "d", false, ""),
            capture("duplicate-pcm", None, "c", false, "2026-11-11"),
        ];
        assert_eq!(unique_indices(&identities), vec![1, 2, 3]);
    }
}
