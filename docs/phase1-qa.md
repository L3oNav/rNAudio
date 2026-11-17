# Phase 1 QA: hardware smoke + soak checklists

Everything here runs on the approved reference machine. Offline correctness is
already covered by the automated suite (`cargo xtask check`).

## Real-hardware smoke (both 128- and 256-frame blocks)

1. `cargo build -p l3o-daemon -p l3o-cli`
2. Start the daemon: `./target/debug/l3od`
3. `l3o devices` — note the mic, the default output, and their rates
   (especially any non-48 kHz device, which exercises the resampler).
4. `l3o daemon start` → `engine: running`
5. Speak into the mic; verify monitor output (headphones) tracks mic level:
   `l3o daemon status` shows bus 4 `peak`/`rms` moving.
6. Play audio from any app; verify bus 5 (System) `peak`/`rms` moves and the
   monitor mix contains it.
7. `l3o daemon set-bus 4 --gain-db -12 --mute` → monitor audibly drops within
   ~10 ms, no click.
8. `l3o daemon set-send 4 --mix stream --off` — no audible monitor change
   (stream mix is not wired to a device until Phase 3; the command must still
   be accepted).
9. Non-48 kHz device: if step 3 showed e.g. 44.1 kHz, confirm capture and
   output both sound correct and `callback avg/max` in status stay well under
   the block deadline (2.67 ms at 128 frames / 48 kHz).
10. `l3o daemon stop` → `engine: stopped`; daemon still answers `status`.

## Failure injection (recovery criteria)

- Unplug the mic mid-stream: bus 4 `STALLED`, monitor keeps playing the other
  buses; plug back in → bus 4 recovers without restarting the daemon.
- Kill the audio server (PipeWire) and restart it: `l3o daemon status` must
  keep answering; `stop` then `start` returns to `running`.
- `pkill -STOP` a poller... (not applicable — pollers are internal); instead
  starve the system source by suspending the app producing audio and confirm
  only bus 5 degrades.

## Eight-hour soak (Phase 1 runs a 60-minute minimum)

1. `l3o daemon start`, then loop `l3o daemon status --watch` (or poll every
   minute) to a log.
2. Play audio continuously; talk into the mic periodically.
3. After 60 minutes (8 hours for the release gate) check the log:
   - daemon process alive, `engine: running` throughout;
   - `callback avg/max` stable and below the 50%-of-deadline target;
   - `source underruns` explained only by intentional starvation;
   - memory flat (RSS sampled in the log).

## Latency measurement method (baseline for regression)

Round-trip = device-buffer estimate (`l3o latency`) + `engine latency` from
`l3o daemon status` + one frame of resampler delay. Record for both block
sizes; hardware loopback impulse measurement (played tone re-captured) is the
follow-up before the Phase 1 gate.
