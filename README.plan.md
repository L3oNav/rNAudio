# L3O Audio development plan

**Source of truth:** `master.md`
**Starting point:** M0 is complete; this plan covers M1–M7 without repeating completed scaffolding.
**Last verified:** 2026-11-15

## Current verified baseline

Using the documented Nix environment:

- Formatting, clippy with warnings denied, and all **23 tests pass**.
- `cargo deny check` **fails** on [RUSTSEC-2026-0293](https://rustsec.org/advisories/RUSTSEC-2026-0293): `ringbuf 0.4.8`, inherited through `flexaudio 0.8.1`, has a panic-safety double-free/use-after-free advisory. Actual reachability in L3O has not yet been established.
- The mixer routing matrix exists, but send gains are currently applied directly during processing in `crates/l3o-engine/src/lib.rs:138`; they need the same smoothed, RT-safe control path as other parameters.
- `l3od` remains a stub, and capture, effect chains, PipeWire routing, GUI, and packaging are not implemented.

The dependency advisory is therefore **P1.1 and a Phase 1 approval blocker**, not merely a future release task.

## Delivery rules applying to every task

All acceptance criteria below are **required** unless explicitly marked otherwise.

- **TDD and coverage:** write failing behavioral tests first where automation is applicable; reach at least **80% coverage for testable workspace code**, with any platform-specific exclusions documented and approved. Coverage does not replace audio-routing, failure-recovery, or hardware tests.
- **Standard checks:** `cargo xtask check` and full `cargo deny check` must pass. Release builds use the lockfile. `cpal` stays aligned with `flexaudio`; currently that means `cpal 0.16` and one `alsa-sys`.
- **Real-time safety:** after preparation, audio callbacks perform no allocation, locking, logging, blocking I/O, or panicking operations. Control changes use bounded handoff mechanisms, and amplitude changes use 5–20 ms ramps.
- **Security:** validate all IPC, preset, configuration, and routing inputs. Local control is same-user only. Services run without elevated privileges. No unresolved critical/high security findings may pass a phase gate.
- **Privacy:** audio is not recorded or transmitted by default. Diagnostics containing process names, executable paths, or device names require user initiation and redaction.
- **Review:** Rust code review for implementation tasks; additional security review for IPC, persistence, platform routing, packaging, and permissions.
- **Documentation:** update README capability claims and `master.md` status at every phase gate. Do not advertise planned functionality as available.
- **Performance:** use 48 kHz stereo and both 128- and 256-frame blocks. A proposed engine target is **p99.9 callback time below 50% of the block deadline** on approved reference hardware; this target and the hardware require approval before Phase 1 completion.

A single maintainer may perform several approval roles, but product, engineering, QA, and security signoffs should be recorded separately.

## Phase overview

| Phase | Master scope | Outcome |
|---|---|---|
| 1 | M1 + minimal daemon control | Secure, measurable, real-time headless audio engine |
| 2 | M2 | Complete built-in MVP DSP and per-bus chains |
| 3 | M3 | Linux application routing and independent OBS Stream output |
| 4 | M4 + production control/persistence | GUI and complete Streamer Mode |
| 5 | M5 | Approved Linux release and operational support |
| 6 | M6–M7 | Independently gated macOS and Windows releases |

**Schedule caveat:** `master.md` gives a 6–8 week Linux MVP headline, but its individual M1–M5 estimates total **9–11 weeks**. Use 9–11 weeks as the initial planning range, re-estimate after Phase 1, and do not commit to a date until dependency remediation and performance targets are approved.

---

## Phase 1 — Security baseline and real-time engine

**Goal:** A long-lived headless daemon can capture audio, process the mixer, and play it reliably with secure local control.

### P1.1 — Remediate dependencies and strengthen CI

**Acceptance criteria**

- A fixed compatible `flexaudio`/`ringbuf` dependency line or reviewed MIT fork is adopted; `cargo deny check` passes without suppressing RUSTSEC-2026-0293. Reachability through `Consumer::skip`/`clear` is documented.
- `cargo tree -i alsa-sys` shows one copy; the `cpal`/`flexaudio` alignment remains intact. Locked CI builds pass.
- Linux CI runs formatting, clippy, tests, coverage, and full `cargo deny`; macOS and Windows run offline tests as well as compilation.
- CI uses read-only repository permissions and immutable, reviewed action pins.

**Limits**

- No general dependency refresh, replacement audio framework, or unrelated feature work.
- No license relaxation or permanent advisory suppression to obtain a green build.

### P1.2 — Approve and implement RT-safe mixer controls

**Acceptance criteria**

- Engineering approves documented graph ownership, queue boundaries, control snapshots, failure states, and clock assumptions.
- Automated tests prove every bus gain/mute/send and master gain/mute transition uses a 5–20 ms click-free ramp, including the currently direct send-gain path.
- An instrumented test detects **zero allocations after preparation** during representative mixer processing.
- Duplicate bus IDs, non-finite parameters, unsupported channel counts, and malformed control state are rejected before reaching the callback; existing routing tests remain green.
- Reference hardware, callback budget, latency measurement method, and single-versus-multiple microphone scope are approved.

**Limits**

- Gain remains the only effect.
- No device routing, GUI, virtual sinks, or DSP-chain implementation.

### P1.3 — Connect capture, conversion, engine, and output

**Acceptance criteria**

- On a real Linux setup, microphone and system-loopback sources each flow through the actual engine to a selected physical output.
- Offline tests cover supported sample formats, device-rate conversion to 48 kHz, mono-to-stereo duplication, partial blocks, and queue overflow/underflow.
- Both 128- and 256-frame configurations pass sample-level routing tests and real-hardware smoke tests without changing channel destinations.

**Limits**

- No per-application routing, null sinks, 5.1, or macOS/Windows hardware validation.
- Do not assume CPAL/ALSA and PipeWire share a clock without measurement.

### P1.4 — Establish the daemon and minimal secure local IPC

**Acceptance criteria**

- `l3od` remains running without a client and supports clean engine start, stop, status, and runtime-parameter operations.
- The Linux control socket is confined to the user session, uses restrictive permissions and same-user peer verification, and exposes no network listener.
- Versioned requests have schema validation, size limits, timeouts, and bounded responses; malformed-message and fuzz tests cause no panic, unbounded read, or state mutation.
- CLI failures return nonzero status without exposing stack traces or sensitive process information.

**Limits**

- Unix socket only; no remote administration, GUI, autostart, or stable public IPC compatibility promise.
- No persisted preset migration in this task.

### P1.5 — Add recovery, telemetry, latency measurement, and phase QA

**Acceptance criteria**

- Simulated `StreamStalled`, frame gaps, device removal, and output failure reopen or degrade only the affected source; unrelated sources continue and the daemon remains alive.
- Clock-domain simulations show bounded queue behavior; adaptive resampling is exercised for independent-clock sources.
- Peak/RMS, CPU, queue fill, underruns, and measured round-trip latency are available outside the audio thread.
- A 60-minute reference-hardware run at both block sizes has zero crashes, zero RT allocations after warmup, bounded memory, and no unexplained engine-attributable underruns.

**Limits**

- No universal latency guarantee across hardware.
- No production macOS/Windows drift backend or per-application routing.

**Phase 1 gate:** Engineering approves the RT/clock design; security approves the local IPC threat model; the dependency advisory is resolved; performance measurements become the regression baseline.

---

## Phase 2 — Built-in DSP and per-bus effect chains

**Goal:** Every bus supports the MVP effects defined in `master.md`, with offline correctness and performance evidence.

### P2.1 — Introduce serializable, preallocated effect chains

**Acceptance criteria**

- Each bus supports ordered built-in effects, bypass, reset, and control-thread reordering without callback allocation.
- Tests verify processing order, state reset, parameter validation, stable bus IDs, and old-preset deserialization.
- Aggregate `latency_frames` equals the approved sum/alignment behavior of the contained effects.

**Limits**

- No LV2/CLAP/VST3 hosting, arbitrary executable/plugin paths, GUI rack, or v1.1 effects.

### P2.2 — Implement EQ, filters, pan, and stereo tools

**Acceptance criteria**

- Gain/pan, configurable 10–31-band parametric EQ, HP/LP, width, and balance pass approved reference tests using impulse, sine, noise, silence, null, reset, and parameter-extreme inputs.
- Parameter changes are click-free, filter reset leaves no prior-state leakage, and reported latency matches measurements.
- Finite validated inputs and extreme supported parameters do not produce unbounded non-finite output.

**Limits**

- No AutoEQ/EqualizerAPO import, convolution, reverb, or automatic room correction.

### P2.3 — Implement dynamics with explicit latency behavior

**Acceptance criteria**

- Compressor and gate pass threshold, ratio, attack, release, hold, and envelope-reference tests at both block sizes.
- The lookahead limiter respects its approved sample ceiling for defined post-lookahead test signals and reports its lookahead exactly.
- Bypass/reset and parameter automation produce no discontinuities beyond approved signal tolerances.

**Limits**

- No multiband compressor, loudness/maximizer, autotune, or unapproved true-peak guarantee.

### P2.4 — Complete the MVP microphone chain

**Acceptance criteria**

- Integration tests verify this order: **flexaudio denoise → gate → EQ → compressor → de-esser → limiter**.
- Missing denoise capability, permission denial, or backend failure produces a reported, controlled fallback without stopping other buses.
- Offline tests cover the native chain, and a real microphone test confirms monitor/stream routing without unintended monitor echo.

**Limits**

- One microphone for the Linux MVP unless the open scope decision changes.
- No new denoiser, multi-input aggregation, or auxiliary-input UI.

### P2.5 — Add analysis, benchmarks, and DSP documentation

**Acceptance criteria**

- Peak, RMS, LUFS, and spectrum measurements pass known-signal tests with documented tolerances.
- Criterion benchmarks cover every expensive effect and complete microphone/bus chains; approved callback budgets pass on reference hardware.
- Allocation tests cover complete chains, and each effect documents parameters, smoothing, reset behavior, and latency.

**Limits**

- No graphical meters yet, no certified loudness-meter claim, and no portable performance guarantee from one benchmark machine.

**Phase 2 gate:** DSP/engineering approval of references and tolerances; all MVP effects pass the offline matrix and complete-chain RT checks.

---

## Phase 3 — Linux PipeWire routing and OBS output

**Goal:** Applications route to the six buses without duplicate playback, and OBS captures exactly the independent Stream mix.

### P3.1 — Implement owned PipeWire virtual-device lifecycle

**Acceptance criteria**

- An unprivileged user can idempotently create monitorable null sinks for all six stable bus IDs and the separate `L3O Stream` sink.
- Restart and cleanup tests remove only nodes owned by the current L3O instance; foreign nodes and user routes remain unchanged.
- Duplicate creation, stale owned-node records, and PipeWire unavailability return explicit states rather than panics.

**Limits**

- No root service, system-wide PipeWire configuration, arbitrary node deletion, or third-party device management.

### P3.2 — Connect sink monitors to the correct engine inputs

**Acceptance criteria**

- Distinguishable synthetic signals sent to each sink reach only their designated bus in sample-level integration tests.
- Moving an application stream into a bus removes its original physical-output path; tests detect both missing and doubled audio.
- Actual graph clock behavior is measured and recorded rather than inferred from API choice.

**Limits**

- Do not replace the specified Linux monitor model with per-process capture.
- No protected-audio or unsupported-stack bypass.

### P3.3 — Add application movement, persistent rules, and watcher behavior

**Acceptance criteria**

- Users can move active application streams and persist executable/process-tree rules; newly appearing streams follow the approved precedence policy.
- Unknown applications fall back to `System`.
- Tests cover application exit, restart, PID reuse, duplicate/conflicting rules, and representative browser/team-client process trees.
- Rules are validated data, never shell commands; rejected rules do not change active routing.

**Limits**

- Universal routing of protected or unusual audio stacks is not promised.
- No arbitrary process control beyond the required audio routing operations.

### P3.4 — Deliver independent `L3O Stream` output

**Acceptance criteria**

- OBS captures the `L3O Stream` sink monitor while Monitor remains on the selected physical output.
- Sample-level tests prove excluded buses are absent, included buses are present, and no path is doubled.
- End-to-end Monitor/Stream latency and representative OBS A/V offset are measured and documented.

**Limits**

- No custom OBS plug-in, virtual kernel driver, or fixed A/V-sync guarantee across uncontrolled hardware.

### P3.5 — Harden recovery and document routing policy

**Acceptance criteria**

- PipeWire restart, daemon termination, clean shutdown, device removal, and application churn follow an approved restoration/cleanup policy without foreign-node changes.
- Headless PipeWire tests run in CI where feasible; physical-output and OBS tests are recorded for the release matrix.
- Documentation identifies process-tree, protected-audio, and crash-restoration limitations.

**Limits**

- No guarantee to reconstruct arbitrary routing changes made outside L3O after a crash.

**Phase 3 gate:** Product and engineering approve route precedence/restoration; all six buses pass concurrent distinguishable-signal tests with zero cross-bus or duplicate-path failures.

---

## Phase 4 — GUI and complete Streamer Mode

**Goal:** A native GUI controls the independent daemon, effects, routing, presets, and both mixes without interrupting audio when closed.

### P4.1 — Version and harden production IPC, configuration, and presets

**Acceptance criteria**

- Bounded, versioned messages validate IDs, string/collection lengths, finite values, and supported operations; unknown versions and malformed requests fail without mutation.
- Configuration writes are atomic, symlink-safe, and appropriately permissioned; malformed files preserve the last known good state and produce an actionable error.
- Old preset fixtures deserialize with explicit defaults and stable bus IDs; upgrade and rollback compatibility tests pass.

**Limits**

- Local same-user control only; no remote API, scripting shell, plug-in loading, or silent destructive migration.

### P4.2 — Create the native GUI and mixer/master views

**Acceptance criteria**

- Using the approved toolkit, six strips expose fader, mute, meters, and both sends; two masters expose gain and mute.
- Closing and reopening the GUI leaves loopback audio continuous and restores current daemon state.
- Daemon disconnect, reconnect, empty-device, and permission-denied states have tested, actionable UI behavior.

**Limits**

- Implement only one UI toolkit—`egui` is recommended but still requires approval.
- The GUI process must not own the engine.

### P4.3 — Implement rack, applications, presets, and settings views

**Acceptance criteria**

- Users can reorder/bypass MVP effects, save/load presets, drag applications onto buses, edit routing rules, and change devices, buffer, theme, and shortcuts.
- Rejected operations and daemon failures leave UI and daemon state reconciled, with no phantom routing or partially applied presets.
- Integration tests cover every control through IPC to its observable engine/platform effect.

**Limits**

- No v1.1/v2 effects, 5.1 controls, multiple UI toolkits, or unsupported routing claims.

### P4.4 — Complete Streamer controls, tray, and global panic behavior

**Acceptance criteria**

- An automated matrix verifies independent send volumes/mutes, master mutes, “hear the stream,” and mic-to-stream-only without monitor echo.
- Enabling and disabling “hear the stream” follows the approved routing semantics and restores the previous Monitor state.
- Global panic reaches its approved safe state within one control transfer plus the configured 5–20 ms gain ramp; it cannot be accidentally reset by stale UI state.
- Tray or global-hotkey registration failure is reported without stopping audio or disabling in-app panic controls.

**Limits**

- Linux release behavior only.
- No feedback-prone implicit routing or unapproved panic definition.

### P4.5 — Complete UX, localization, accessibility, E2E QA, and documentation

**Acceptance criteria**

- All user-facing strings have EN/ES entries; missing localization keys fail tests.
- Meters animate at 30–60 fps on reference hardware without violating audio callback budgets.
- Primary workflows are keyboard-operable, with visible focus and reviewed contrast; any native screen-reader limitations are documented.
- E2E tests cover mixer → DSP → app routing → OBS, GUI closure without an audio gap, daemon/client recovery, malformed presets, and every Streamer matrix case.
- Product approves UX and Streamer semantics; security approves IPC/configuration handling.

**Limits**

- No analytics, additional languages, visual redesign, or substitution of manual review for automatable routing tests.

**Phase 4 gate:** Complete Linux user journey passes with no unresolved release-blocking UX, correctness, or security defects.

---

## Phase 5 — Linux release, approval, and post-launch support

**Goal:** A reproducible, supportable Linux MVP is released only after clean-install, security, and endurance evidence exists.

### P5.1 — Harden daemon lifecycle and first-run onboarding

**Acceptance criteria**

- Fresh start, missing PipeWire, permission denial, device loss, crash/restart, and upgrade paths have tested behavior.
- The user service starts without the GUI; onboarding selects working devices and verifies Monitor and Stream paths.
- No step requires elevated privileges or silently changes unrelated audio configuration.

**Limits**

- No root daemon, security-policy bypass, or macOS/Windows service implementation.

### P5.2 — Deliver the Nix package and NixOS module

**Acceptance criteria**

- A locked build produces the package from a clean checkout.
- A clean NixOS VM test enables PipeWire, the user daemon, and approved RTKit limits, then verifies routing and GUI/daemon separation.
- Upgrade and rollback tests preserve readable configuration and do not overwrite unrelated audio settings.

**Limits**

- No system-wide capture service or unrelated NixOS audio configuration.

### P5.3 — Deliver AppImage and Flatpak packages

**Acceptance criteria**

- Each format installs and runs on at least one approved clean VM with documented minimal PipeWire permissions.
- DSP, application routing, Monitor output, and OBS Stream capture pass the same functional smoke suite in both formats.
- Uninstall tests leave no running service or unintended virtual devices; packages include license notices and dependency provenance.

**Limits**

- No macOS/Windows packages, app-store submission, or claim that every distribution is validated.

### P5.4 — Run release security, QA, and the eight-hour soak

**Acceptance criteria**

- The release candidate completes eight hours with all buses, representative effects, application churn, OBS, and hotplug events at both block settings.
- The run has zero crashes, audio stalls, unbounded resource growth, or unresolved engine-attributable underruns.
- Unit, integration, E2E, IPC/configuration fuzzing, unsafe-code review, coverage, performance, and full dependency checks pass.
- No unwaived high/critical advisory or release-blocking defect remains.

**Limits**

- This validates the published test matrix, not every device or protected application.
- No macOS/Windows release claim.

### P5.5 — Publish documentation, obtain go/no-go approval, and establish support

**Acceptance criteria**

- Install, upgrade, uninstall, OBS, privacy, security, troubleshooting, and known-limitation documentation matches the shipped behavior.
- Product, engineering, QA, and security record go/no-go approval; release artifacts have checksums and a rehearsed rollback procedure.
- A named owner approves severity definitions, dependency-advisory monitoring, triage cadence, patch procedure, and rollback triggers.
- A synthetic support drill demonstrates report → redacted diagnostics → triage → patch/rollback without using real user audio or sensitive process data.

**Limits**

- No unapproved SLA, support dates, silent telemetry, or default process/device-data collection.
- Publishing external artifacts still requires explicit release authorization.

**Phase 5 gate:** Packages pass clean-install tests, the soak and security gates pass, rollback is rehearsed, and support ownership is active before general availability.

---

## Phase 6 — Independent macOS and Windows rollouts

**Goal:** Reuse the common engine and conformance suite without allowing one platform’s feasibility problems to block another platform’s release.

### P6.1 — Run the macOS feasibility/design spike

**Acceptance criteria**

- On macOS 14.2+, demonstrate process taps, original-audio muting, TCC grant/denial, CoreAudio output, aggregate-device/drift requirements, and candidate OBS delivery.
- Engineering approves one release architecture; licensing review explicitly approves or rejects any third-party component.

**Limits**

- No BlackHole bundling, HAL plug-in, or older-macOS support before separate technical/licensing approval.

### P6.2 — Implement and validate the macOS backend

**Acceptance criteria**

- Process routing uses `flexaudio` taps without duplicating capture implementations.
- Original audio is suppressed as approved; independent mixes reach their destinations.
- Adaptive drift correction, hotplug, permission revocation, process-tree fallback, and real OBS capture pass the common conformance suite.

**Limits**

- No own HAL plug-in or kernel driver in this task.
- No promise for protected audio.

### P6.3 — Package, release, and support macOS independently

**Acceptance criteria**

- `.app`/`.pkg` installation passes with hardened runtime and only required entitlements; signing and notarization succeed.
- Clean install, upgrade, rollback, and the same eight-hour security/QA soak pass.
- Platform documentation and support procedures identify actual limitations and ownership.

**Limits**

- No unapproved GPL component bundling and no Windows work hidden in this gate.

### P6.4 — Run the Windows suppression/output decision spike

**Acceptance criteria**

- A recorded test matrix compares session mute, per-app endpoint policy, existing virtual cables, and OBS application capture for duplication, reliability, permissions, latency, security, and maintainability.
- Product, engineering, and security approve the production path and explicitly defer or separately authorize driver work.

**Limits**

- No production dependency on undocumented APIs without explicit approval.
- No ACX/PortCls implementation, Microsoft portal submission, or EV-signing commitment.

### P6.5 — Implement and validate the approved Windows backend

**Acceptance criteria**

- On Windows 10 2004+, per-process capture/routing, approved duplicate suppression, adaptive resampling, hotplug recovery, and Stream delivery pass common conformance tests and real OBS tests.
- Unsupported/protected sessions follow documented fallback behavior without bypass attempts or unrelated-session disruption.

**Limits**

- No signed virtual driver unless separately planned and approved.

### P6.6 — Package, release, and support Windows independently

**Acceptance criteria**

- The selected MSI or NSIS installer passes clean install, upgrade, uninstall, and rollback tests.
- Package authenticity/signing, license notices, and dependency provenance are verified.
- The eight-hour soak and security/QA gate pass without Linux/macOS regressions; support documentation and procedures are published.

**Limits**

- ACX/PortCls development, Microsoft portal submission, and EV driver signing require a separate approved plan.

**Phase 6 gates:** macOS and Windows each require separate architecture and release approvals. A failed Windows spike must not block the Linux or macOS release.

---

## Decisions required at the stated gates

1. **Immediate:** fixed upstream dependency versus reviewed `flexaudio` fork/patch for the `ringbuf` advisory.
2. **Before P1.2 completion:** reference hardware, CPU/latency budgets, and microphone count. Recommended MVP default: one microphone.
3. **Before Phase 3 implementation:** routing-rule precedence and clean-shutdown/crash route-restoration policy.
4. **Before Phase 4 implementation:** UI toolkit—recommended `egui`—plus exact “hear the stream” and panic-mute semantics.
5. **Before release:** support owner and triage cadence; no response-time SLA is assumed.
6. **During Phase 6 spikes:** macOS Stream delivery and Windows suppression/output architecture.
7. **Documentation cleanup:** the project already declares `MIT OR Apache-2.0` in `Cargo.toml` and README, while `master.md` still lists licensing as open. Confirm that this decision is final; third-party BlackHole/plug-in licensing remains separate.

Optional ChatMix HID, 5.1, v1.1 effects, v2 plug-in hosting, and a signed Windows driver are outside this plan.

## Handoff

**Ready to start:** P1.1 dependency remediation and CI hardening.
**Architecture review required before:** P1.2 RT/control implementation.
**Product clarification required before:** Phase 3 routing policy and Phase 4 Streamer semantics—not before all initial work.
