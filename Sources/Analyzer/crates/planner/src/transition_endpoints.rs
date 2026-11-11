//! Musical changes that bound a sustained combination. These propose endpoints;
//! the mapped candidate still has to pass timing, phrase, vocal and layer checks.
use mixlib_types::TacticTrackAnalysis;
use serde_json::Value;

#[derive(Clone, Debug)]
pub(crate) struct Endpoint {
    pub frame: u64,
    pub reason: String,
}

fn number(v: &Value, key: &str) -> Option<f64> {
    v.get(key)?.as_f64().filter(|v| v.is_finite() && *v >= 0.)
}

fn pattern(row: &Value) -> Option<[f64; 16]> {
    let start = row.get("start_frame")?.as_u64()?;
    let end = row.get("end_frame")?.as_u64()?;
    if start >= end {
        return None;
    }
    let mut bins = [0.; 16];
    for onset in row.get("stems")?.get("drums")?.get("onsets")?.as_array()? {
        let at = onset.get("frame")?.as_u64()?;
        if at >= start && at < end {
            let bin = ((at - start) as f64 * 16. / (end - start) as f64).round() as usize % 16;
            bins[bin] += number(onset, "strength")?;
        }
    }
    let sum = bins.iter().sum::<f64>();
    if sum <= 1e-9 {
        return None;
    }
    Some(bins.map(|v| v / sum))
}

fn distance(a: &[f64; 16], b: &[f64; 16]) -> f64 {
    a.iter().zip(b).map(|(a, b)| (a - b).abs()).sum::<f64>() / 2.
}

fn drum_changes(rows: &[Value]) -> Vec<Endpoint> {
    let mut result = vec![];
    for window in rows.windows(4) {
        if window
            .windows(2)
            .any(|w| w[0].get("end_frame") != w[1].get("start_frame"))
        {
            continue;
        }
        let values = window
            .iter()
            .map(|row| {
                let drums = row.get("stems")?.get("drums")?;
                Some((
                    number(drums, "rms")?,
                    number(drums, "onset_density_per_second")?,
                ))
            })
            .collect::<Option<Vec<_>>>();
        let Some(v) = values else {
            continue;
        };
        let energy = (v[0].0 + v[1].0) / 2.;
        let activity = (v[0].1 + v[1].1) / 2.;
        let steady_before = v[..2]
            .iter()
            .all(|&(e, _)| (e - energy).abs() <= energy.max(0.003) * 0.30);
        let down = steady_before
            && energy >= 0.003
            && v[2..]
                .iter()
                .all(|&(e, a)| e <= energy * 0.70 && a <= activity * 0.70);
        // A return often follows silence plus a pickup, not a steady quiet bed.
        let prior_energy = v[0].0.max(v[1].0).max(0.003);
        let prior_activity = v[0].1.max(v[1].1).max(0.25);
        let up = v[2..]
            .iter()
            .all(|&(e, a)| e >= prior_energy * 1.6 && a >= prior_activity * 1.4);
        let changed_pattern = window
            .iter()
            .map(pattern)
            .collect::<Option<Vec<_>>>()
            .is_some_and(|p| {
                distance(&p[0], &p[1]) <= 0.35
                    && distance(&p[2], &p[3]) <= 0.35
                    && p[..2]
                        .iter()
                        .all(|a| p[2..].iter().all(|b| distance(a, b) >= 0.60))
            });
        if down || up || changed_pattern {
            if let Some(frame) = window[2].get("start_frame").and_then(Value::as_u64) {
                result.push(Endpoint {
                    frame,
                    reason: if down {
                        "sustained drum reduction"
                    } else if up {
                        "sustained drum arrival"
                    } else {
                        "sustained drum-pattern change"
                    }
                    .into(),
                });
            }
        }
    }
    result
}

pub(crate) fn arrivals(record: &TacticTrackAnalysis) -> Vec<Endpoint> {
    measured(record)
        .into_iter()
        .filter(|e| {
            e.reason == "sustained drum arrival"
                || record
                    .signal_evidence
                    .get("events")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .any(|v| {
                        v.get("frame").and_then(Value::as_u64) == Some(e.frame)
                            && matches!(
                                v.get("kind").and_then(Value::as_str),
                                Some("drum_return" | "drop")
                            )
                            && number(v, "score").is_some_and(|s| (0.6..=1.).contains(&s))
                    })
        })
        .collect()
}

/// Legacy build spans sometimes include the first full-volume release bar.
/// Refine only when a sustained drum arrival and a stable full-mix plateau
/// identify that final bar as the release, rather than a continuing crescendo.
pub(crate) fn build_release(record: &TacticTrackAnalysis, start: u64, end: u64) -> u64 {
    let Some(rows) = record.signal_evidence.get("bars").and_then(Value::as_array) else {
        return end;
    };
    build_release_rows(rows, start, end)
}

fn build_release_rows(rows: &[Value], start: u64, end: u64) -> u64 {
    let changes = drum_changes(rows);
    for w in rows.windows(2) {
        let Some(at) = w[0].get("start_frame").and_then(Value::as_u64) else {
            continue;
        };
        if at <= start
            || w[0].get("end_frame").and_then(Value::as_u64) != Some(end)
            || w[1].get("start_frame").and_then(Value::as_u64) != Some(end)
            || !changes
                .iter()
                .any(|e| e.frame == at && e.reason == "sustained drum arrival")
        {
            continue;
        }
        if let Some((a, b)) = w[0]
            .get("source")
            .and_then(|s| number(s, "rms"))
            .zip(w[1].get("source").and_then(|s| number(s, "rms")))
        {
            if a > 0.003 && (b - a).abs() <= a * 0.15 {
                return at;
            }
        }
    }
    end
}

pub(crate) fn measured(record: &TacticTrackAnalysis) -> Vec<Endpoint> {
    let rows = record
        .signal_evidence
        .get("bars")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let mut result = drum_changes(rows);
    for event in record
        .signal_evidence
        .get("events")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let kind = event.get("kind").and_then(Value::as_str).unwrap_or("");
        if !["drop", "drum_return", "build", "natural_ending"].contains(&kind)
            || !number(event, "score").is_some_and(|v| v >= 0.20)
        {
            continue;
        }
        if let Some(frame) = event
            .get(if kind == "build" {
                "end_frame"
            } else {
                "frame"
            })
            .and_then(Value::as_u64)
        {
            result.push(Endpoint {
                frame: if kind == "build" {
                    build_release(
                        record,
                        event.get("frame").and_then(Value::as_u64).unwrap_or(frame),
                        frame,
                    )
                } else {
                    frame
                },
                reason: format!("measured {kind} boundary"),
            });
        }
    }
    result.sort_by_key(|p| p.frame);
    result.dedup_by_key(|p| p.frame);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn row(i: u64, quiet: bool, shifted: bool) -> Value {
        let onsets =
            [0, 25, 50, 75].map(|at| json!({"frame":i*100+at+if shifted{12}else{0},"strength":1.}));
        json!({"start_frame":i*100,"end_frame":(i+1)*100,"stems":{"drums":{
            "rms":if quiet {0.02}else{0.06},"onset_density_per_second":if quiet{0.5}else{3.},
            "onsets":onsets}}})
    }

    #[test]
    fn ending_follows_the_measured_change_not_a_power_of_two_duration() {
        for at in [5, 7, 11] {
            let rows = (0..at + 3)
                .map(|i| row(i, i >= at, false))
                .collect::<Vec<_>>();
            assert_eq!(
                drum_changes(&rows)
                    .iter()
                    .map(|e| e.frame)
                    .collect::<Vec<_>>(),
                [at * 100]
            );
        }
    }

    #[test]
    fn build_release_is_refined_only_after_the_crescendo_has_plateaued() {
        let mut rows = (0..5).map(|i| row(i, i < 2, false)).collect::<Vec<_>>();
        for r in &mut rows {
            r["source"] = json!({"rms":0.07});
        }
        assert_eq!(build_release_rows(&rows, 0, 300), 200);
        rows[3]["source"]["rms"] = json!(0.12);
        assert_eq!(
            build_release_rows(&rows, 0, 300),
            300,
            "continuing crescendo cannot be shortened"
        );
        rows[3]["source"]["rms"] = json!(0.07);
        rows[3]["stems"]["drums"]["rms"] = json!(0.01);
        assert_eq!(
            build_release_rows(&rows, 0, 300),
            300,
            "one loud fill is not a release"
        );
    }

    #[test]
    fn return_can_follow_uneven_quiet_pickup_but_not_an_isolated_loud_bar() {
        let mut rows = (0..6).map(|i| row(i, i < 2, false)).collect::<Vec<_>>();
        rows[0]["stems"]["drums"]["rms"] = json!(0.0001);
        assert!(drum_changes(&rows)
            .iter()
            .any(|e| e.frame == 200 && e.reason == "sustained drum arrival"));
        rows[3] = row(3, true, false);
        assert!(!drum_changes(&rows).iter().any(|e| e.frame == 200));
    }

    #[test]
    fn a_single_fill_is_not_a_sustained_change_but_a_new_rhythm_is() {
        let rows = (0..12).map(|i| row(i, i == 5, false)).collect::<Vec<_>>();
        assert!(drum_changes(&rows).is_empty());
        let rows = (0..12).map(|i| row(i, false, i >= 7)).collect::<Vec<_>>();
        assert_eq!(drum_changes(&rows)[0].frame, 700);
        assert!(drum_changes(&rows)[0].reason.contains("pattern"));
    }
}
