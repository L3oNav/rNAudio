//! The prepared engine must not allocate on the render path: draining
//! controls, processing every bus, and finishing the mixes perform zero
//! heap allocations. Enforced with a counting global allocator.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use l3o_core::{BusId, MixId, Preset, Send};
use l3o_engine::{Control, Engine, MixAccumulator};

static ALLOCS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        // Safety: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // Safety: forwarded unchanged to the system allocator.
        unsafe { System.dealloc(ptr, layout) };
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn no_allocations_after_prepare() {
    const SR: u32 = 48_000;
    const BLOCK: usize = 256;

    let preset = Preset::default();
    let mut engine = Engine::new(&preset, SR, 2).unwrap();
    engine.prepare(BLOCK);
    let mut sender = engine.take_control_sender().unwrap();

    let mut out = MixAccumulator::new(BLOCK, 2);
    let mut inputs: Vec<Vec<f32>> = preset
        .buses
        .iter()
        .map(|_| vec![0.01f32; BLOCK * 2])
        .collect();

    // Warmup (not measured): first touches after preparation.
    engine.drain_controls();
    for (i, input) in inputs.iter_mut().enumerate() {
        engine.process(BusId(i as u32), input, &mut out);
    }
    engine.finish(&mut out);

    sender
        .send(Control::SetBusGain {
            id: BusId(0),
            gain_db: -3.0,
            muted: false,
        })
        .unwrap();
    sender
        .send(Control::SetBusSend {
            id: BusId(1),
            mix: MixId::Stream,
            send: Send::disabled(),
        })
        .unwrap();
    sender
        .send(Control::SetMaster {
            master: l3o_core::Master::default(),
        })
        .unwrap();
    drop(sender);

    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..100 {
        out.clear();
        engine.drain_controls();
        for (i, input) in inputs.iter_mut().enumerate() {
            engine.process(BusId(i as u32), input, &mut out);
        }
        engine.finish(&mut out);
    }
    let after = ALLOCS.load(Ordering::Relaxed);

    assert_eq!(
        before,
        after,
        "mixer processing allocated {} times after prepare",
        after - before
    );
}
