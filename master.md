# L3O Audio — Master Plan

**A cross-platform Rust audio mixer / DSP suite: Easy Effects-style processing plus SteelSeries Sonar-style channel routing and Streamer Mode.**

Status: M0 complete (workspace, dev shell, smoke tests)
Target platforms: Linux (primary), macOS, Windows
Last updated: 2026-10-30

---

## 1. Current state of the repository

- Cargo project with a hello-world `src/main.rs`, `rustfmt.toml`, and a stub `Cargo.toml`.
- Declared dependencies: `cpal = "0.18.2"` and `flexaudio = "0.8.1"`.

Findings that shape the design:

- **`flexaudio` 0.8.1 is capture-only** (microphone, system loopback, per-process) on Linux/Windows/macOS, MIT licensed. It does not play audio and does not create devices.
- **The dependency graph does not resolve today**: `flexaudio` pulls `cpal 0.16` (`alsa-sys 0.3`), while the project declares `cpal 0.18.2` (`alsa-sys 0.4`). Cargo fails on the `links = "alsa"` conflict.
- **Fix (done in M0):** the workspace pins `cpal = "0.16"`, the version flexaudio uses, so `alsa-sys` is unified. The root manifest is now the virtual workspace.

---

## 2. Product definition

A desktop application with:

1. **Channel mixer (buses):** `Game`, `Chat`, `Media`, `Aux`, `Mic`, plus a catch-all `System`/rest bus.
2. **Per-channel effects rack** (Easy Effects-style): parametric EQ, compressor, gate, limiter, de-esser, reverb, bass enhancer, stereo tools, etc.
3. **Streamer Mode:** every channel sends independently to two mixes:
   - **Personal/Monitor** → your headphones.
   - **Stream** → what the audience hears (OBS).
   - Independent volume and mute per mix, "hear what the audience hears", mic to stream only (no echo), global panic mute.
4. **Per-application routing** (drag & drop), with no dependency on SteelSeries GG or its hardware. Optional: read the headset's **ChatMix** dial over HID.

Signal flow:

```
Apps ─► Bus Game  ─► [EQ·Comp·…] ─► ┬─ send Monitor ─┐
Apps ─► Bus Chat  ─► [EQ·Comp·…] ─► ├─ send Stream  ─┤
Apps ─► Bus Media ─► [EQ·Comp·…] ─► │                ├─► Master Monitor ─► Headphones
Apps ─► Bus Aux   ─► [EQ·Comp·…] ─► │                └─► Master Stream  ─► "L3O Stream" ─► OBS
Mic  ─► Bus Mic   ─► [Denoise·Gate·EQ·Comp·DeEss·Lim] ┘
```

---

## 3. Key technical decision: capture and routing without drivers (v1)

Sonar's model is "each app selects a virtual device". That requires drivers on Windows and macOS. The modern alternative is **per-process capture and remixing**, which `flexaudio` already provides. Use a hybrid model per platform:

| Platform | Bus capture | Avoid duplicate audio | Stream output to OBS |
|---|---|---|---|
| **Linux** (priority) | PipeWire null sink per bus (`module-null-sink`), captured with `SystemLoopback` + `device_id` | Move the app's stream into the null sink (no physical output) | "L3O Stream" sink; OBS captures its *monitor* |
| **macOS 14.2+** | Per-app process taps (`flexaudio` per-process) | `CATapMuteBehavior::mutedWhenTapped` (mutes the original) | OBS captures the L3O process (ScreenCaptureKit) or a dedicated HAL plugin |
| **Windows 10 2004+** | Per-app WASAPI process loopback | **Spike:** session mute (`ISimpleAudioVolume`) vs. per-app endpoint (`IAudioPolicyConfigFactory`, undocumented) vs. virtual cable | OBS "Application Audio Capture" of the L3O process, or a virtual cable |

This keeps **Linux and macOS driverless**. Windows is the only place where a signed driver vs. relying on VB-CABLE/VoiceMeeter must eventually be decided. That is a phase-3 decision and does not block the MVP.

**Extra advantage on Linux:** capture and playback live in the same PipeWire graph, so there is **no clock drift** between buses and monitor. Windows/macOS do drift and need correction (see §5).

---

## 4. Proposed Rust stack

| Layer | Choice | Notes |
|---|---|---|
| Capture | **`flexaudio` 0.8** (pinned) | Mic, loopback, per-process, process list, VAD and RNNoise denoise included |
| Linux output | **`pipewire`** 0.10 | Target streams by node (null sinks) and create nodes; cpal/ALSA cannot do this |
| Windows/macOS output | **`cpal` 0.16** | Aligned with flexaudio; avoids the `alsa-sys` conflict |
| Real-time plumbing | `rtrb`, `ringbuf`, `triple_buffer`, `atomic_float` | No locks, no allocations in the audio callback |
| Resampling / drift | `rubato` | Already in flexaudio's dependency tree |
| FFT / spectrum | `rustfft` | Analyzer and convolver |
| Loudness | `ebur128` | LUFS metering; gain applied by our own code |
| Convolver | `fft-convolver` or a custom overlap-save | IRs for reverb / cabinet simulation |
| Pitch | `signalsmith-stretch` | Phase 2 |
| Config | `serde` + `toml` | Presets and profiles |
| IPC | `tokio` + `serde_json` over a Unix socket / named pipe | Daemon ↔ UI |
| UI | **`eframe`/`egui`** + `egui_plot` | Pure Rust, easy meters and curves; Tauri is the alternative if a web UI is preferred |
| Tray / hotkeys | `tray-icon`, `global-hotkey` | |
| Logging | `tracing` | |
| Packaging | `cargo-wix`/NSIS, `cargo-bundle` + notarization, Nix flake | |

**Alternative to evaluate in phase 2:** host **LV2/CLAP** plugins with `livi` to reuse LSP/Calf on Linux and reach real Easy Effects parity without reimplementing 20 effects. Risks: GPL licensing of some plugins and packaging complexity. Recommendation: native DSP for the core, plugin host afterwards.

---

## 5. Workspace architecture

```
Cargo.toml                 # workspace
crates/
  l3o-core/                # model: Bus, Mix, Send, Preset, config, IPC protocol
  l3o-dsp/                 # effects + analysis (pure, offline-testable)
  l3o-io/                  # flexaudio wrapper, output backends, clocks, drift
  l3o-engine/              # RT graph, buses, mixes, smoothed parameters
  l3o-platform/            # linux/ (PipeWire sinks + routing), macos/, windows/
  l3o-daemon/              # binary l3od (engine, no UI)
  l3o-gui/                 # binary l3o (egui)
  l3o-cli/                 # presets, diagnostics, headless mode
xtask/                     # packaging, installers, Nix
nix/                       # flake, package, NixOS module
```

**Real-time engine rules:**

- Fixed block size (128/256 frames), canonical 48 kHz stereo (mono mic is duplicated; 5.1 is deferred).
- Capture → SPSC ring per bus → engine. UI parameters go through a `triple_buffer`; gains use 5–20 ms ramps.
- **Drift:** PI controller on queue fill level + adaptive `rubato` resampler per source (Windows/macOS only; Linux shares the clock).
- Atomic telemetry: peak/RMS/LUFS, CPU, underruns, latency.
- `flexaudio` watchdog (`StreamStalled`, `frame_index` gaps) → reopen a source without interrupting the rest.

---

## 6. DSP: realistic scope

| Phase | Effects |
|---|---|
| **MVP** | Gain/pan, 10–31 band parametric EQ, HP/LP, compressor, gate, lookahead limiter, stereo width/balance, meters + spectrum |
| **Mic (MVP)** | flexaudio denoise, gate, EQ, compressor, de-esser, limiter |
| **v1.1** | Reverb, delay, bass enhancer, exciter, IR convolver, multiband compressor, loudness/maximizer, autotune |
| **v2** | LV2/CLAP host, AutoEQ / EqualizerAPO curve import, optional VST3 |

Every effect implements: `prepare(sr, block)`, `process(&mut [f32; N])`, `reset()`, smoothed params, `latency()`. All covered by offline tests (impulse, sine, noise, null tests) and `criterion` benchmarks.

---

## 7. UI / UX

- **Mixer:** six strips with fader, mute, meter, two send buttons (Monitor / Stream) and a volume per mix.
- **Rack:** one tab per channel, draggable effect chain, bypass, presets.
- **Apps:** process list with drag & drop onto channels, rules by executable / bundle id, watcher for newly launched apps.
- **Streamer:** two masters with mute/volume, "hear the stream", panic, clip indicators.
- **Settings:** devices, buffer/latency, theme, language (EN/ES), shortcuts, autostart.
- Easy Effects-style look: dark theme, large knobs, interactive EQ curves, live spectrum at 30–60 fps.
- **UI and engine separated from day one** (daemon + GUI): closing the window must not stop audio, and it enables the tray.

---

## 8. Phases and estimates (1 developer, full time)

| Phase | Content | Estimate |
|---|---|---|
| **M0 Foundations** | Fix `cpal` 0.16, workspace, CI (clippy/fmt/deny), Nix devShell, test tone through output, latency measurement | 3–5 days |
| **M1 Engine** | Buses, rings, clocks/drift, parameters, telemetry, hotplug | 2–3 weeks |
| **M2 DSP v1** | EQ, comp, gate, limiter, stereo, meters, mic chain; tests | 2 weeks |
| **M3 Linux routing** | Null sinks per bus, move apps, rules, watcher, monitor capture, "L3O Stream" sink | 2 weeks |
| **M4 Streamer Mode** | Two mixes, sends, mutes, "hear stream", mic stream-only, presets, shortcuts, tray | 1–2 weeks |
| **M5 Linux release** | Daemon/IPC, onboarding, config, i18n, Nix package + NixOS module, AppImage/Flatpak, 8 h soak test | 2 weeks |
| **M6 macOS** | Process taps with mute, aggregate device, TCC permissions, CoreAudio output, BlackHole vs. own HAL plugin decision, notarization | 3–4 weeks |
| **M7 Windows** | Per-process suppression spike, per-app endpoint, virtual cable detection, OBS app capture; ACX/PortCls driver + EV signing decision | 4–8 weeks (+ driver separately) |

**Linux MVP: ~6–8 weeks.** macOS and Windows afterwards, once the platform abstraction exists.

---

## 9. Risks and mitigations

| Risk | Impact | Mitigation |
|---|---|---|
| Windows virtual driver (EV signing, Microsoft portal, cost) | High | v1 without a driver: process loopback + OBS app capture; detect VB-CABLE/VoiceMeeter; spike before committing |
| Suppressing the original audio on Windows | High | Test session mute with process loopback; if it fails, per-app endpoint (undocumented API) or a virtual cable |
| `flexaudio` is very young (0.8.x, ~3.5k downloads) | Medium | Wrap it behind our own trait, pin the version, fork (MIT) if necessary |
| `alsa-sys` conflict | Blocking | Pin `cpal 0.16` now |
| Apps with process trees (Chrome, Teams) or protected audio | Medium | Rules by executable + tree; document limits; fall back to the System bus |
| Latency / A/V sync with OBS | Medium | Configurable buffer, measure real latency, avoid long-lookahead DSP on the monitor path |
| macOS: HAL plugin + notarization | Medium | v1 with taps + OBS app capture; own plugin or BlackHole (GPL) later |
| Reimplementing Easy Effects is huge | Medium | Prioritize 8 effects; LV2 host in v2 |

---

## 10. Packaging and testing

- **NixOS:** flake with a `devShell` (rust, clang, pkg-config, pipewire, alsa-lib), `package`, and a module that enables the user daemon, `services.pipewire`, and RT limits (`security.rtkit`).
- **Generic Linux:** AppImage/Flatpak with PipeWire permission.
- **Windows:** MSI/NSIS; later a signed driver package.
- **macOS:** `.app` + `.pkg`, hardened runtime, microphone/capture entitlements, notarization.
- **CI:** Linux (Nix) + Windows + macOS runners; `cargo test`, `clippy -D warnings`, `cargo deny`; offline audio tests and an 8 h soak.

---

## 11. Open decisions

1. **Linux first?** Assumed yes (development machine is NixOS). If Windows is the priority, reorder phases and tackle the driver earlier.
2. **UI:** `egui` (native, fast to iterate, "technical" look) or Tauri + React (prettier, more work, webview)? egui is recommended for the MVP.
3. **Windows:** accept "use an existing virtual cable / OBS captures L3O" in v1 and defer a signed driver to v2, or invest in an own driver from the start?
4. **License:** MIT/Apache-2.0. Bundling BlackHole on macOS would force GPLv3 or require permission.
5. **Microphone scope:** a single input or several (mic + aux in)?
