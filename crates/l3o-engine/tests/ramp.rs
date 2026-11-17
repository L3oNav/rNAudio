//! Every mixer control transition must use a 5–20 ms click-free ramp.
//!
//! Drives the engine with DC input, applies one control through the real
//! control queue, and checks the effective gain trajectory: linear slope
//! (no clicks), and convergence inside the required window.

use l3o_core::{BusId, Master, MixId, Preset, Send};
use l3o_engine::{Control, Engine, MixAccumulator};

const SR: u32 = 48_000;
const BLOCK: usize = 128;
const RAMP_FRAMES: usize = 480; // 10 ms at 48 kHz
const MAX_RAMP_MS: f32 = 20.0;
const MIN_RAMP_MS: f32 = 5.0;

struct Trajectory {
    /// Effective gain (output / input) of the first monitor channel, per frame.
    gain: Vec<f32>,
}

/// Runs blocks of DC through the engine, applying `control` after one steady
/// block, and records the chosen mix's gain trajectory per frame.
fn trajectory(preset: &Preset, control: Control, mix: MixId) -> Trajectory {
    let mut engine = Engine::new(preset, SR, 2).unwrap();
    engine.prepare(BLOCK);
    let mut sender = engine.take_control_sender().unwrap();

    let mut out = MixAccumulator::new(BLOCK, 2);
    let mut input = vec![1.0f32; BLOCK * 2];

    let mut step = |engine: &mut Engine, gain: &mut Vec<f32>| {
        out.clear();
        input.fill(1.0); // fresh capture block, like the render loop provides
        engine.drain_controls();
        engine.process(BusId(0), &mut input, &mut out);
        engine.finish(&mut out);
        let buf = match mix {
            MixId::Monitor => &out.monitor,
            MixId::Stream => &out.stream,
        };
        for frame in buf.chunks(2) {
            gain.push(frame[0]);
        }
    };

    let mut gain = Vec::new();
    step(&mut engine, &mut gain);
    sender.send(control).unwrap();
    drop(sender);

    let blocks_needed = RAMP_FRAMES.div_ceil(BLOCK) + 3;
    for _ in 1..blocks_needed {
        step(&mut engine, &mut gain);
    }
    Trajectory { gain }
}

fn check_ramp(traj: &Trajectory, from: f32, to: f32) {
    // Ramp starts after the first block.
    let start = BLOCK;
    let g = &traj.gain;

    // 1. Convergence: reaches the target within the 5–20 ms window.
    let end = g[start..]
        .iter()
        .position(|g| (*g - to).abs() < 1e-4)
        .map(|i| i + start)
        .expect("trajectory must converge");
    let ramp_ms = (end - start) as f32 / SR as f32 * 1000.0;
    assert!(
        (MIN_RAMP_MS..=MAX_RAMP_MS).contains(&ramp_ms),
        "ramp duration {ramp_ms} ms outside {MIN_RAMP_MS}..={MAX_RAMP_MS} ms"
    );

    // 2. Click-free: per-frame slope never exceeds the linear ramp slope
    //    (plus float tolerance), from the frame after the control onward.
    let step = (to - from) / RAMP_FRAMES as f32;
    let mut max_delta = 0.0f32;
    for w in g[start..].windows(2) {
        max_delta = max_delta.max((w[1] - w[0]).abs());
    }
    assert!(
        max_delta <= step.abs() + 1e-6,
        "click detected: max per-frame delta {max_delta}, ramp slope {step}"
    );

    // 3. Steady before and after: exact endpoints.
    assert!(
        (g[start - 1] - from).abs() < 1e-6,
        "ramp started before control"
    );
    assert!((g[end] - to).abs() < 1e-4, "did not reach target");
}

#[test]
fn bus_gain_down_is_ramped() {
    let preset = Preset::default(); // bus 0 at unity
    let traj = trajectory(
        &preset,
        Control::SetBusGain {
            id: BusId(0),
            gain_db: -12.0,
            muted: false,
        },
        MixId::Monitor,
    );
    check_ramp(&traj, 1.0, l3o_core::db_to_linear(-12.0));
}

#[test]
fn bus_mute_is_ramped() {
    let preset = Preset::default();
    let traj = trajectory(
        &preset,
        Control::SetBusGain {
            id: BusId(0),
            gain_db: 0.0,
            muted: true,
        },
        MixId::Monitor,
    );
    check_ramp(&traj, 1.0, 0.0);
}

#[test]
fn bus_send_disable_is_ramped() {
    let preset = Preset::default();
    let traj = trajectory(
        &preset,
        Control::SetBusSend {
            id: BusId(0),
            mix: MixId::Stream,
            send: Send::disabled(),
        },
        MixId::Stream,
    );
    check_ramp(&traj, 1.0, 0.0);
}

#[test]
fn bus_send_gain_up_is_ramped() {
    let mut preset = Preset::default();
    preset.bus_mut(BusId(0)).unwrap().sends.monitor = Send::disabled();
    let traj = trajectory(
        &preset,
        Control::SetBusSend {
            id: BusId(0),
            mix: MixId::Monitor,
            send: Send::UNITY,
        },
        MixId::Monitor,
    );
    check_ramp(&traj, 0.0, 1.0);
}

#[test]
fn master_monitor_gain_is_ramped() {
    let preset = Preset::default();
    let traj = trajectory(
        &preset,
        Control::SetMaster {
            master: Master {
                monitor_gain_db: -6.0,
                ..Master::default()
            },
        },
        MixId::Monitor,
    );
    check_ramp(&traj, 1.0, l3o_core::db_to_linear(-6.0));
}

#[test]
fn master_stream_mute_is_ramped() {
    let preset = Preset::default();
    let traj = trajectory(
        &preset,
        Control::SetMaster {
            master: Master {
                stream_muted: true,
                ..Master::default()
            },
        },
        MixId::Stream,
    );
    check_ramp(&traj, 1.0, 0.0);
}
