# L3O Audio

Cross-platform audio mixer and DSP suite for Linux, macOS and Windows:
Easy Effects-style processing, SteelSeries Sonar-style channel routing, and a
Streamer Mode with independent **Monitor** and **Stream** mixes.

The full plan lives in [`master.md`](master.md).

## Status

**M0 — foundations complete.** The workspace builds, the Nix dev shell provides
the native audio libraries, and the CLI can enumerate devices/processes, print
backend capabilities, play a test tone and report buffer/latency information.

## Quick start

```sh
nix develop
cargo build --workspace
cargo test --workspace

cargo run -p l3o-cli -- status
cargo run -p l3o-cli -- devices
cargo run -p l3o-cli -- processes
cargo run -p l3o-cli -- caps
cargo run -p l3o-cli -- latency
cargo run -p l3o-cli -- tone --hz 440 --secs 1
```

Without Nix, install `pkg-config`, `clang`, PipeWire headers and ALSA headers,
then use `cargo` directly.

## Workspace

| Crate | Responsibility |
|---|---|
| `l3o-core` | Domain model: buses, sends, mixes, presets, dB helpers |
| `l3o-dsp` | Real-time `Processor` trait and gain node |
| `l3o-io` | cpal playback and flexaudio capture discovery |
| `l3o-engine` | Mixer graph feeding the Monitor and Stream mixes |
| `l3o-platform` | Virtual-device backends (PipeWire first) |
| `l3o-daemon` | `l3od`, the engine daemon |
| `l3o-cli` | `l3o`, CLI and smoke tests |
| `xtask` | Workspace automation (`cargo xtask check`) |

## Design constraints

- `cpal` is pinned to **0.16** to match `flexaudio`; mixing versions breaks the
  `alsa-sys` `links = "alsa"` constraint.
- The engine runs at 48 kHz stereo. Device-rate conversion happens at the I/O
  boundary (M1).
- Nothing allocates or locks on the audio callback path.
