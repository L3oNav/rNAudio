# Real-time and clock design (Phase 1)

This document is the engineering reference for the mixer control path. It is
the artefact the Phase 1 gate reviews; update it when the design changes.

## Graph ownership

- The `Engine` is owned by exactly one thread: the render thread. No other
  thread holds a reference to it and no lock ever guards it.
- All bus/master state lives inside `Engine`. There is no shared mutable
  mixer state outside it.

## Queue boundaries

- Control changes travel over a bounded SPSC queue (`rtrb`, capacity 64)
  from any non-RT thread to the engine. The producer side validates every
  message before enqueueing.
- The render thread drains the queue exactly once per block, at block start,
  before processing any bus. Control changes therefore apply at block
  granularity and never mid-block.
- Queue full: `ControlError::QueueFull` is returned to the sender. The
  engine keeps its current state; the sender decides to retry or drop. The
  engine is never partially updated.

## Control snapshots

- A control message is a complete value (`SetBusGain`, `SetBusSend`,
  `SetMaster`). The engine applies it by setting new `Gain` targets; ramps
  run inside the block processor.
- `apply_preset` remains a render-thread call: it snapshots a whole `Preset`
  into the runtime in one pass.

## Failure states

- Rejected before the callback: duplicate bus IDs, non-finite gain values,
  channel counts outside `1..=MAX_CHANNELS`, malformed control messages.
  Construction and control-send return `Result`; nothing panics on the
  render path.
- Unknown bus ID in a control message: ignored, counted as
  `ignored_controls`, engine state untouched.
- Gain targets are clamped to `0.0..=16.0` (+24 dB) inside the `Gain` node.

## Ramps

- Every amplitude change (bus gain/mute, send gain/enable, master gain/mute)
  goes through the `Gain` node with a 10 ms time constant, inside the
  required 5–20 ms window, and is click-free (no per-sample discontinuity
  beyond the ramp slope).

## Allocation freedom

- After `prepare(max_block_frames)`, one block of mixer processing
  (drain controls, all buses, master) performs zero heap allocations. All
  scratch is preallocated in `prepare`. This is enforced by a counting-
  allocator test.

## Recovery model

- A starved or erroring capture source degrades only itself: its block is
  zero-padded, `underflows`/`stalled` telemetry moves, and the rest of the
  graph (other sources, the engine, the output) keeps running. When chunks
  resume, the source recovers on its own — nothing restarts.
- Device-level reopen for a dying capture backend is flexaudio's job (its
  stream recovers across backend restarts per its contract); the graph
  tolerates the gaps that produces.
- Output failure is reported through the cpal error callback and the daemon
  stays alive; the operator restarts the engine via `EngineStop`/`Start`.

## Render callback allocation note

The mixer core is allocation-free after preparation (counting-allocator
test). The I/O boundary currently has one documented exception: per-chunk
channel adaptation may grow a scratch buffer once when a capture source
first delivers a larger-than-seen chunk. Rate conversion itself
(`LinearResampler`) is fully preallocated. This exception is removed when
device selection lands in P1.4; rate conversion upgrades from linear
interpolation to rubato in P1.5.

## Clock assumptions

- The engine processes only at `CANONICAL_SAMPLE_RATE` (48 kHz) stereo.
  Device-rate conversion happens at the I/O boundary (Phase 1.3).
- Capture and output devices are assumed to have independent clocks unless
  measured otherwise; drift handling is a Phase 1.5 concern, not assumed
  away.
