use mixlib_audio::*;
use mixlib_types::{Cancellation, EmergencyReason, SourceEditKind, TacticId};

const RATE: u32 = 8000;
fn tone(count: usize, frequency: f64) -> Vec<[f32; 2]> {
    (0..count)
        .map(|i| {
            let v =
                (0.2 * (i as f64 * frequency * std::f64::consts::TAU / RATE as f64).sin()) as f32;
            [v, -v]
        })
        .collect()
}
fn cache(id: &str, start: u64, pcm: Vec<[f32; 2]>) -> VerifiedRecoveryPcm {
    let digest = recovery_pcm_digest(RATE, start, &pcm);
    VerifiedRecoveryPcm::verify(
        id.into(),
        RATE,
        start,
        pcm,
        &digest,
        &Cancellation::default(),
    )
    .unwrap()
}
fn failure() -> LiveSourceFailure {
    LiveSourceFailure {
        reason: EmergencyReason::UnexpectedSourceLoss,
        sample_rate_hz: RATE,
        source_id: "outgoing".into(),
        source_frame: 8000,
        output_frame: 20000,
        failed_condition: "mounted audio source disconnected during playback".into(),
    }
}
fn next(delay: u64) -> ReadyRecoverySource {
    ReadyRecoverySource {
        pcm: cache("incoming", 32000, tone(8000, 880.)),
        landing_output_frame: 20000 + delay,
    }
}
fn render(mut renderer: LiveRecoveryRenderer, chunks: usize) -> (Vec<[f32; 2]>, LiveRecoveryAudit) {
    let mut output = vec![];
    loop {
        let mut block = vec![[9.; 2]; chunks];
        let count = renderer
            .render_into(&mut block, &Cancellation::default())
            .unwrap();
        output.extend_from_slice(&block[..count]);
        assert!(block[count..].iter().all(|s| *s == [0.; 2]));
        if count < chunks {
            break;
        }
    }
    (output, renderer.audit().clone())
}

#[test]
fn valid_cached_continuation_wins_even_when_dramatic_effects_are_permitted() {
    let pcm = tone(16000, 440.);
    let healthy = cache("outgoing", 0, pcm.clone());
    let mut policy = LiveRecoveryPolicy::conservative(RATE);
    policy.permitted_motion = vec![TacticId::E03, TacticId::E02, TacticId::E01];
    let renderer = LiveRecoveryRenderer::activate(
        failure(),
        Some(&healthy),
        next(4000),
        &policy,
        &Cancellation::default(),
    )
    .unwrap();
    let (audio, audit) = render(renderer, 127);
    assert_eq!(audit.route, LiveRecoveryRoute::PreparedContinuation);
    assert!(audit.activation.is_none());
    assert!(audit.motion.is_none());
    assert_eq!(audio[123], pcm[8123]);
    assert_eq!(audio[4100], tone(8000, 880.)[100]);
    assert_eq!(audit.next_source_resume_frame, 40000);
    assert_eq!(audit.output_end_frame, 32000);
    assert_eq!(audit.state, LiveRecoveryState::Completed);
}

#[test]
fn missing_original_audio_uses_ready_minimal_handoff_without_capture() {
    let renderer = LiveRecoveryRenderer::activate(
        failure(),
        None,
        next(0),
        &LiveRecoveryPolicy::conservative(RATE),
        &Cancellation::default(),
    )
    .unwrap();
    let (audio, audit) = render(renderer, 512);
    assert_eq!(audit.route, LiveRecoveryRoute::MinimalFade);
    assert_eq!(audit.activation.as_ref().unwrap().healthy_buffer_frames, 0);
    assert_eq!(audio[0], [0.; 2]);
    assert_eq!(audio[100], tone(8000, 880.)[100]);
    assert_eq!(audio.len(), 8000);
    assert!(audit.healthy_capture_digest.is_none());
    assert_eq!(audit.rejected_safe_recoveries.len(), 1);
}

#[test]
fn all_three_emergency_gestures_render_healthy_capture_and_end_before_next_vocals() {
    let history = cache("outgoing", 0, tone(8000, 440.));
    let mut signatures = vec![];
    for id in [TacticId::E01, TacticId::E02, TacticId::E03] {
        let mut policy = LiveRecoveryPolicy::conservative(RATE);
        policy.permitted_motion = vec![id];
        let renderer = LiveRecoveryRenderer::activate(
            failure(),
            Some(&history),
            next(6400),
            &policy,
            &Cancellation::default(),
        )
        .unwrap();
        let (audio, audit) = render(renderer, 137);
        assert_eq!(audit.route, LiveRecoveryRoute::Motion(id));
        let activation = audit.activation.as_ref().unwrap();
        assert_eq!(activation.reason, EmergencyReason::UnexpectedSourceLoss);
        assert_eq!(activation.activated_output_frame, 20000);
        assert_eq!(activation.rejected_safe_recoveries.len(), 2);
        assert!(activation.next_source_verified);
        let edit = audit.motion.as_ref().unwrap();
        assert_eq!(edit.source_end_frame, 8000);
        assert!(edit.source_start_frame < edit.source_end_frame);
        assert_eq!(edit.output_end_frame, 24000);
        assert!(matches!(
            (id, &edit.kind),
            (TacticId::E01, SourceEditKind::Brake { .. })
                | (TacticId::E02, SourceEditKind::Backspin { .. })
                | (TacticId::E03, SourceEditKind::Scratch { .. })
        ));
        assert!(audio[100..3900].iter().any(|v| v[0].abs() > 0.05));
        assert!(audio[4000..6400].iter().all(|v| *v == [0.; 2]));
        assert_eq!(audio[6500], tone(8000, 880.)[100]);
        assert!(audio.iter().all(|v| v[0].is_finite() && v[1] == -v[0]));
        assert!(audit.peak <= 0.891251);
        assert_eq!(audit.vocal_guard_frames, 2400);
        signatures.push(audio[100..200].to_vec());
        serde_json::to_string(&audit).unwrap();
    }
    assert!(
        signatures.windows(2).all(|w| w[0] != w[1]),
        "brake, backspin and scratch must produce different motion"
    );
}

#[test]
fn effect_policy_cannot_invent_missing_pcm_or_override_a_short_minimal_fade() {
    let history = cache("outgoing", 0, tone(8000, 440.));
    let mut policy = LiveRecoveryPolicy::conservative(RATE);
    assert_eq!(
        LiveRecoveryRenderer::activate(
            failure(),
            Some(&history),
            next(6400),
            &policy,
            &Cancellation::default()
        )
        .err()
        .unwrap()
        .code,
        "RECOVERY_UNAVAILABLE"
    );
    policy.permitted_motion = vec![TacticId::E01];
    assert_eq!(
        LiveRecoveryRenderer::activate(
            failure(),
            None,
            next(6400),
            &policy,
            &Cancellation::default()
        )
        .err()
        .unwrap()
        .code,
        "RECOVERY_UNAVAILABLE"
    );
    let renderer = LiveRecoveryRenderer::activate(
        failure(),
        Some(&history),
        next(80),
        &policy,
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(renderer.audit().route, LiveRecoveryRoute::MinimalFade);
    policy.permitted_motion = vec![TacticId::M01];
    assert_eq!(
        LiveRecoveryRenderer::activate(
            failure(),
            Some(&history),
            next(6400),
            &policy,
            &Cancellation::default()
        )
        .err()
        .unwrap()
        .code,
        "RECOVERY_POLICY"
    );
}

#[test]
fn corrupt_nonfinite_or_mismatched_pcm_is_rejected_before_any_effect_capture() {
    let pcm = tone(8000, 440.);
    let digest = recovery_pcm_digest(RATE, 0, &pcm);
    let mut changed = pcm.clone();
    changed[99][0] += 0.001;
    assert_eq!(
        VerifiedRecoveryPcm::verify(
            "outgoing".into(),
            RATE,
            0,
            changed,
            &digest,
            &Cancellation::default()
        )
        .err()
        .unwrap()
        .code,
        "RECOVERY_HASH"
    );
    let mut bad = pcm.clone();
    bad[42][0] = f32::NAN;
    assert_eq!(
        VerifiedRecoveryPcm::verify(
            "outgoing".into(),
            RATE,
            0,
            bad,
            &digest,
            &Cancellation::default()
        )
        .err()
        .unwrap()
        .code,
        "RECOVERY_PCM"
    );
    assert_eq!(
        VerifiedRecoveryPcm::verify(
            "outgoing".into(),
            RATE,
            1,
            pcm,
            &digest,
            &Cancellation::default()
        )
        .err()
        .unwrap()
        .code,
        "RECOVERY_HASH"
    );
    let unrelated = cache("another-source", 0, tone(8000, 440.));
    assert_eq!(
        LiveRecoveryRenderer::activate(
            failure(),
            Some(&unrelated),
            next(0),
            &LiveRecoveryPolicy::conservative(RATE),
            &Cancellation::default()
        )
        .err()
        .unwrap()
        .code,
        "RECOVERY_SOURCE"
    );
}

#[test]
fn decode_failure_chooses_a_renderable_permitted_motion_in_the_correct_timebase() {
    let history = cache("outgoing", 5000, tone(3000, 440.));
    let mut failure = failure();
    failure.reason = EmergencyReason::UnexpectedDecodeFailure;
    failure.failed_condition = "decoder reported a malformed block during playback".into();
    let mut policy = LiveRecoveryPolicy::conservative(RATE);
    policy.permitted_motion = vec![TacticId::E02, TacticId::E01];
    let renderer = LiveRecoveryRenderer::activate(
        failure.clone(),
        Some(&history),
        next(6400),
        &policy,
        &Cancellation::default(),
    )
    .unwrap();
    assert_eq!(
        renderer.audit().route,
        LiveRecoveryRoute::Motion(TacticId::E01)
    );
    assert_eq!(
        renderer.audit().activation.as_ref().unwrap().reason,
        EmergencyReason::UnexpectedDecodeFailure
    );
    failure.sample_rate_hz = 48000;
    assert_eq!(
        LiveRecoveryRenderer::activate(
            failure,
            Some(&history),
            next(6400),
            &policy,
            &Cancellation::default()
        )
        .err()
        .unwrap()
        .code,
        "RECOVERY_FAILURE"
    );
}

#[test]
fn cancellation_stops_processing_and_clears_stale_output() {
    let cancel = Cancellation::default();
    let mut renderer = LiveRecoveryRenderer::activate(
        failure(),
        None,
        next(0),
        &LiveRecoveryPolicy::conservative(RATE),
        &cancel,
    )
    .unwrap();
    let mut block = [[0.; 2]; 128];
    renderer.render_into(&mut block, &cancel).unwrap();
    cancel.cancel();
    block.fill([8.; 2]);
    assert_eq!(
        renderer
            .render_into(&mut block, &cancel)
            .err()
            .unwrap()
            .code,
        "CANCELLED"
    );
    assert!(block.iter().all(|v| *v == [0.; 2]));
    assert_eq!(renderer.audit().state, LiveRecoveryState::Cancelled);
    assert_eq!(renderer.audit().rendered_frames, 128);
    assert_eq!(
        renderer
            .render_into(&mut block, &Cancellation::default())
            .err()
            .unwrap()
            .code,
        "CANCELLED"
    );
    assert_eq!(
        LiveRecoveryRenderer::activate(
            failure(),
            None,
            next(0),
            &LiveRecoveryPolicy::conservative(RATE),
            &cancel
        )
        .err()
        .unwrap()
        .code,
        "CANCELLED"
    );
}

#[test]
fn a_second_source_loss_drains_only_verified_cache_and_releases_to_silence() {
    let mut renderer = LiveRecoveryRenderer::activate(
        failure(),
        None,
        next(0),
        &LiveRecoveryPolicy::conservative(RATE),
        &Cancellation::default(),
    )
    .unwrap();
    let mut block = [[0.; 2]; 200];
    renderer
        .render_into(&mut block, &Cancellation::default())
        .unwrap();
    renderer
        .notify_next_source_loss("incoming filesystem disappeared after preparation".into())
        .unwrap();
    let (audio, audit) = render(renderer, 31);
    assert_eq!(audit.state, LiveRecoveryState::CachedNextExhausted);
    assert_eq!(audio.len(), 7800);
    assert_eq!(*audio.last().unwrap(), [0.; 2]);
    assert!(audio[..7700].iter().any(|v| v[0].abs() > 0.1));
    assert!(audio[7770..].iter().all(|v| v[0].abs() < 0.065));
    assert!(audit.next_source_loss.is_some());
}

#[test]
fn bounded_preparation_applies_linked_headroom_gain_to_actual_output() {
    let next = ReadyRecoverySource {
        pcm: cache("incoming", 32000, vec![[2., -1.]; 8000]),
        landing_output_frame: 20000,
    };
    let renderer = LiveRecoveryRenderer::activate(
        failure(),
        None,
        next,
        &LiveRecoveryPolicy::conservative(RATE),
        &Cancellation::default(),
    )
    .unwrap();
    let (audio, audit) = render(renderer, 1000);
    assert!(audit.applied_gain < 0.5);
    assert!((audio[100][0] as f64 - 10f64.powf(-1. / 20.)).abs() < 1e-6);
    assert_eq!(audio[100][1], -audio[100][0] / 2.);
    assert!(audio
        .iter()
        .flatten()
        .all(|v| v.is_finite() && v.abs() <= 0.891251));
}
