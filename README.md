# L3O Audio

A cross-platform audio mixer and DSP suite for Linux, macOS and Windows:
Easy Effects-style processing per channel, SteelSeries Sonar-style app routing,
and a Streamer Mode that splits everything into two independent mixes — one for
you (**Monitor**) and one for your audience (**Stream**).

The full design and roadmap live in [`master.md`](master.md).

---

## Status

**M0 complete — foundations.**

What works today:

- Cargo workspace with eight crates and a Nix dev shell.
- Output device enumeration and a click-free test tone (cpal 0.16).
- Input/system/process enumeration (flexaudio 0.8.1).
- Platform backend detection (PipeWire).
- Mixer routing matrix: bus gain/mute, per-mix sends, master gains.
- 23 unit tests, clippy-clean, formatted, license audit passing.

Not implemented yet: real-time capture into the engine, per-bus effect chains,
virtual sinks, IPC, GUI, macOS/Windows backends. See [Roadmap](#roadmap).

---

## How it works

```
Apps ─► Bus Game  ─► [effects] ─► ┬─ send Monitor ─┐
Apps ─► Bus Chat  ─► [effects] ─► ├─ send Stream  ─┤
Apps ─► Bus Media ─► [effects] ─► │                ├─► Master Monitor ─► headphones
Apps ─► Bus Aux   ─► [effects] ─► │                └─► Master Stream  ─► "L3O Stream" ─► OBS
Mic  ─► Bus Mic   ─► [effects] ─► ┘
```

- A **bus** is a channel (`Game`, `Chat`, `Media`, `Aux`, `Mic`, `System`).
- Each bus has its own gain, mute, effects and two independent **sends**:
  `Monitor` and `Stream`. This is what makes Streamer Mode work: exclude Chat
  from the stream while still hearing it, or send the mic only to the audience.
- Each **mix** has its own master gain and mute.
- The engine runs at 48 kHz stereo; device rates are converted at the I/O edge.

Routing sources by platform (target design):

| Platform | Bus source | No duplicate audio | Stream delivery |
|---|---|---|---|
| Linux | PipeWire null sink per bus | App streams moved into the null sink | Monitor of the `L3O Stream` sink |
| macOS | Core Audio process taps | `CATapMuteBehavior::mutedWhenTapped` | Process capture / HAL plug-in |
| Windows | WASAPI process loopback | Session mute or per-app endpoint | OBS app capture / virtual cable |

---

## Requirements

- Rust **1.85+** (the dev shell ships a current stable).
- Linux: PipeWire and ALSA development headers, `pkg-config`, `clang`.
- macOS: Xcode command line tools (CoreAudio frameworks).
- Windows: MSVC toolchain (recommended) and the Windows SDK.
- Nix is optional but is the supported development path on NixOS.

---

## Quick start

### With Nix (recommended on NixOS)

```sh
nix develop
cargo build --workspace
cargo test --workspace
cargo run -p l3o-cli -- status
```

The dev shell provides `rustc`, `cargo`, `clippy`, `rustfmt`, `rust-analyzer`,
`pkg-config`, `clang`, `cmake`, PipeWire and ALSA, plus `cargo-deny`.

### Without Nix

Debian/Ubuntu:

```sh
sudo apt install pkg-config clang libclang-dev libpipewire-0.3-dev libasound2-dev
```

Fedora:

```sh
sudo dnf install pkgconf-pkg-config clang clang-devel pipewire-devel alsa-lib-devel
```

Arch:

```sh
sudo pacman -S pkgconf clang pipewire alsa-lib
```

Then:

```sh
cargo build --workspace
```

---

## Running

The CLI binary is `l3o`:

```sh
cargo run -p l3o-cli -- <command>
```

| Command | Purpose |
|---|---|
| `status` | Show version, detected platform backend and library versions |
| `devices` | List output (cpal) and capture/monitor devices (flexaudio) |
| `processes` | List processes that currently own an audio session |
| `caps` | Print flexaudio's capability snapshot (does not open streams) |
| `latency [--device NAME]` | Show format, buffer range and theoretical minimum latency |
| `tone [--hz F] [--secs S] [--device NAME]` | Play a faded sine tone to verify output |

Examples:

```sh
cargo run -p l3o-cli -- devices
cargo run -p l3o-cli -- processes
cargo run -p l3o-cli -- tone --hz 440 --secs 1
cargo run -p l3o-cli -- latency --device "pipewire"
cargo run -p l3o-cli -- status --verbose
```

The daemon stub prints the same platform information:

```sh
cargo run -p l3o-daemon
```

> The latency figure is a lower bound taken from the device buffer range. Real
> round-trip latency measurement arrives with the engine in M1.

---

## Building

### Development build

```sh
nix develop --command cargo build --workspace
```

or, with the native dependencies already installed:

```sh
cargo build --workspace
```

### Release build

```sh
cargo build --release -p l3o-cli -p l3o-daemon
```

Binaries land in `target/release/l3o` and `target/release/l3od`.

### Platform notes

- **Linux:** cpal talks to ALSA; PipeWire is reached through the ALSA or
  PipeWire compatibility layer. `libpipewire-0.3` is required by flexaudio's
  system/process capture backend.
- **macOS:** no extra packages beyond the Xcode tools. Capture uses Core Audio
  process taps (macOS 14.2+).
- **Windows:** build from a Developer PowerShell with the MSVC toolchain.
  Capture uses WASAPI process loopback (Windows 10 2004+).

### Packaging

Not available yet. Nix package/module, AppImage/Flatpak, MSI/NSIS and the macOS
bundle are scheduled for M5–M7. CI currently only builds and tests.

---

## Testing and quality

```sh
cargo xtask check          # fmt --check + clippy -D warnings + tests
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo deny check licenses  # cargo-deny is in the dev shell
```

CI (`.github/workflows/ci.yml`) runs the full suite on Linux and
`cargo check` on Windows and macOS.

Guidelines:

- Audio tests must be **offline** — never require a sound card, so CI stays
  headless. Feed buffers through `Processor`/`Engine` and assert on samples.
- Test the routing matrix (mute, send enable, master gain) for every change.
- Keep `unsafe` out of the workspace unless it is unavoidable and documented.

---

## Project layout

```
Cargo.toml                 # virtual workspace
flake.nix                  # dev shell and, later, packages/modules
master.md                  # product and engineering plan
crates/
  l3o-core/                # domain model: Bus, Send, MixId, Preset, dB helpers
  l3o-dsp/                 # Processor trait, Gain, future effects
  l3o-io/                  # cpal playback, flexaudio capture discovery
  l3o-engine/              # buses -> Monitor/Stream mix routing
  l3o-platform/            # VirtualDevices trait and backend detection
  l3o-daemon/              # l3od (engine process)
  l3o-cli/                 # l3o (CLI and smoke tests)
xtask/                     # workspace automation
.github/workflows/ci.yml   # CI
```

Key invariants:

- `cpal` is pinned to **0.16** because flexaudio depends on it. Two cpal
  versions would link two `alsa-sys` crates and fail with
  `links = "alsa"`. Check with `cargo tree -i alsa-sys`.
- The engine is allocation-free and lock-free on the audio path. All
  cross-thread communication uses preallocated queues (M1).
- UI parameters are smoothed; never write raw atomics straight into samples.

---

## Maintaining

### Dependencies

- `cargo update` then `cargo xtask check`. Commit `Cargo.lock` with the change.
- When bumping `flexaudio`, check its `cpal` requirement first. If it moves to
  0.18, bump the workspace `cpal` in the same commit and run
  `cargo tree -i alsa-sys` to confirm a single copy.
- `cargo deny check licenses` must pass. Add a license to `deny.toml` only after
  confirming the crate's terms.
- Keep the MSRV (`rust-version = "1.85"`) honest: new language features that
  require a newer compiler need a deliberate bump.

### Nix

- Update the pinned nixpkgs with `nix flake update` and run the checks.
- If you add a native dependency, update **both** `flake.nix` and the apt line in
  CI. Otherwise CI and local builds diverge.
- The "Git tree is dirty" warning is expected while `flake.nix` is untracked by
  Git; stage new files before running `nix develop`.

### Releases

1. Bump `[workspace.package] version` in the root `Cargo.toml`.
2. Update `README.md` and `master.md` status/roadmap.
3. Run `cargo xtask check` and `cargo deny check licenses`.
4. Tag `vX.Y.Z`.
5. Build platform packages once M5+ packaging exists.

### Changes to the plan

`master.md` is the source of truth for scope, phases and open decisions. Update
it when a phase, risk or architectural decision changes — not just the code.

---

## Adding features

### A new DSP effect

Implement `l3o_dsp::Processor`:

```rust
use l3o_dsp::{AudioBlock, Processor};

pub struct SoftClip {
    drive: f32,
}

impl Processor for SoftClip {
    fn name(&self) -> &'static str {
        "soft-clip"
    }

    fn prepare(&mut self, _sample_rate: u32, _max_block_frames: usize) {}

    fn process(&mut self, block: &mut AudioBlock<'_>) {
        for sample in block.data.iter_mut() {
            *sample = (*sample * self.drive).tanh();
        }
    }

    fn reset(&mut self) {}
}
```

Rules:

- No allocation, locking, logging or panics inside `process`.
- Reset filter state in `reset`; configure coefficients in `prepare`.
- Report lookahead in `latency_frames` so the engine can compensate.
- Add offline tests (impulse, sine, silence, null test) and a `criterion`
  benchmark if the effect is expensive.
- The per-bus chain that owns effects is introduced in M2; until then, effects
  are validated through offline tests.

### A new mixer bus

Buses are data-driven:

1. Add the channel in `Preset::default_layout` (`crates/l3o-core/src/lib.rs`)
   and give it a stable `BusId`.
2. Update the default preset tests and the README signal-flow diagram.
3. Once the UI exists, add the strip; the engine and persistence pick it up
   automatically because they iterate over `Preset::buses`.

Do not reuse `BusId`s: presets and routing rules reference them.

### A new CLI command

Add a variant to `Command` in `crates/l3o-cli/src/main.rs` and handle it in
`run`. Keep commands non-interactive and exit non-zero on failure so they can be
scripted.

### A new platform backend

1. Implement `l3o_platform::VirtualDevices` in a `cfg`-gated module.
2. Extend `detect()` to report it only when the backend is actually reachable.
3. Keep privileged operations out: null sinks and process taps should work as a
   normal user.
4. Add tests for detection and for the "unsupported" path. Emit
   `PlatformError::Unsupported` instead of panicking.
5. Update the platform table in this README and `master.md`.

Capture backends belong in flexaudio; do not duplicate them here.

### A new persisted setting

- Add the field to the relevant struct in `l3o-core` with `#[serde(default)]`.
- Provide a sensible `Default` and a unit test that an old preset still
  deserializes.
- Wire it through `Engine::apply_preset` (or the equivalent) with smoothing.

---

## Troubleshooting

**`No output device` / empty device list.**
You are probably outside a graphical session (SSH, CI, container). The CLI
needs a reachable audio server.

**ALSA prints `Unknown PCM pulse/jack/oss`.**
Harmless: cpal enumerates every ALSA PCM and those plug-ins are not installed.
It does not affect PipeWire devices.

**`platform backend: unsupported`.**
`detect()` looks for `$XDG_RUNTIME_DIR/pipewire-0` (or `/run/user/<uid>/pipewire-0`).
Start PipeWire in the current user session.

**Device selection fails.**
`--device` matches the exact name printed by `l3o devices`. If two devices share
a name, the first match wins.

**Cargo reports an `alsa-sys` `links = "alsa"` conflict.**
Something pulled a second cpal version. Run `cargo tree -i alsa-sys` and pin
`cpal` back to 0.16.

**`nix develop` says flake.nix is not tracked.**
Stage it (`git add flake.nix` or `git add -N flake.nix`); flakes only see files
known to Git.

**The tone plays but nothing is heard.**
Check the default sink, the device volume and whether the stream was routed to a
null sink or an inactive profile.

---

## Roadmap

| Phase | Scope | Status |
|---|---|---|
| M0 | Workspace, dev shell, device/process discovery, test tone | **Done** |
| M1 | Real-time engine: capture → mix → output, rings, drift, hotplug | Next |
| M2 | DSP v1: EQ, compressor, gate, limiter, mic chain | Planned |
| M3 | Linux routing: null sinks per bus, app rules, `L3O Stream` | Planned |
| M4 | Streamer Mode UI: sends, mutes, presets, shortcuts | Planned |
| M5 | Daemon/IPC, packaging, NixOS module, Linux release | Planned |
| M6 | macOS: process taps, CoreAudio, notarization | Planned |
| M7 | Windows: process loopback, routing, driver decision | Planned |

---

## License

MIT OR Apache-2.0, at your option.
