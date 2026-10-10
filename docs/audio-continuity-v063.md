# v0.6.3 Phase 1: audio continuity fixes

The two Phase 0 software discontinuity paths now have regression coverage: callback starvation/reset can no longer write an instantaneous transition directly to CPAL, and a full jitter buffer no longer clears eight playable frames and jumps its cursor to the incoming packet. These are software fixes, not ESP32 listening acceptance. No firmware, wire protocol, codec, PLC, adaptive rate controller or jitter target increase is included.

## Final-output smoothing

`mic-audio/src/output.rs` contains the allocation-free renderer called directly by the CPAL data callback and by regression tests. It operates after device-rate resampling, before F32/I16/U16 conversion and channel replication. Thus the final CPAL samples, including inactive/prefill zero output, are covered.

Normal output is unchanged and has no added frame/lookahead delay. On input starvation the current sampler is reset immediately, the remainder of that callback stops consuming new PCM, and the last rendered value decays to zero over approximately 3 ms using a smoothstep envelope. The envelope persists across callbacks. Recovery crossfades from the most recent output to fresh PCM over approximately 3 ms; after sustained silence this is an explicit zero-to-audio fade-in. At 48 kHz the envelope is 144 output frames; at 16 kHz it is 48. Actual sample request counts, the existing two-sample resampler lookahead and the existing 320-input-sample prefill are unchanged. A temporarily short callback can wait until its next callback to resume; this is a bounded interruption behavior, not a new fixed normal-playout delay.

Ring entries carry an `upstream_faded` flag from Engine's existing de-click processing. When resuming into already faded PCM, the output renderer corrects the boundary offset and decays that correction instead of multiplying a second gain fade. Healthy jitter/de-click transitions pass through normally; the existing de-click algorithm remains unchanged. Final output is clamped to normalized [-1,1] before format conversion. The additive offset branch can alter a short transient and can clip at full-scale pathological boundaries; the normal TX test signal has substantial headroom. This is a continuity transition, not waveform extrapolation/PLC or recovered lost content.

Playback clear increments a monotonic atomic epoch. Input samples are epoch-tagged, so the callback invalidates interpolation history even if active=false was set and cleared between callbacks. Stale entries are discarded with bounded work; a pending newer-epoch sample is retained across a raced pop. No locks, allocation, file/network I/O, sleep or logging are added to the data callback. Input consumption counters are accumulated once per callback rather than one atomic RMW per input sample. The pre-existing CPAL error callback still maintains output_state separately; it was not redesigned.

## Jitter admission and capacity policy

| Term | Definition |
|---|---|
| Expected Sequence | The next packet slot to consume at an operational jitter tick. Increments modulo u32 even on a missing slot. None means no current anchor. |
| Buffer Occupancy | Number of distinct stored packets, maximum eight. Existing `jitter_buffer_depth` retains this meaning. |
| Contiguous Playable Depth | Number of consecutive stored sequences beginning exactly at Expected; zero if Expected is absent. Exposed by getter and the additive `contiguous_playable_depth` metric. |
| Target Depth | Existing startup occupancy threshold, clamped 2–4. It is not sequence span, contiguous depth, or a guaranteed steady-state latency. It has not been raised or silently redefined. |

Admission rules:

1. Buffered duplicates and recently admitted/delivered duplicates are rejected and counted as duplicates. Seen history is bounded to 64 sequences.
2. A packet behind Expected is late and rejected once playout has started. Existing startup behavior allows a backward adjustment smaller than eight slots before startup. An empty-stall reset can establish a fresh anchor later.
3. Reordering within the accepted forward window is admitted subject to capacity. Positive forward distance is measured with wrapping arithmetic.
4. Forward distance greater than 100, including ambiguous exactly-half-range ordering, is rejected without clearing retained packets or moving Expected. It records `forward_sequence_rejected`. Three empty operational deadlines can still invoke the existing empty-stall reset; source/session resets retain their existing path.
5. At occupancy eight, identify the stored packet farthest forward from Expected using modular distance. If the incoming packet is closer, replace only that farthest packet. Otherwise reject the incoming farther packet. Expected and started state do not change; retained PCM stays on its timeline. An evicted packet is removed from seen history so a later retry before its deadline may be admitted. Rejected capacity/far packets are not marked as admitted duplicates.

Each capacity decision counts one dropped packet and one capacity overrun. For compatibility, the structured `BUFFER_RESYNC` envelope is retained with reasons `jitter_capacity_replace_farthest` and `jitter_capacity_reject_farther`, carrying pre-decision occupancy and Expected. These reasons mean capacity reconciliation **without a cursor reset**; do not interpret every BUFFER_RESYNC count as the former all-packet clear. `forward_sequence_rejected` likewise records a rejected discontinuity rather than a cursor jump. Existing empty/scheduler resync and recovery reasons remain observable.

The canonical `1849..1856 + incoming1857` case now keeps all eight existing packets, expected1849 and contiguous depth8, and rejects only1857. A full `expected3 / keys4..11` buffer receiving3 replaces11, preserves4..10 and plays3 next. The same ordering rules work across u32 wrap. No admission-time packet discontinuity is hidden by smoothing. Capacity still cannot retain nine unique frames; rejecting a farther frame may produce a later visible playout deficit. Sustained TX/RX frequency differences remain unresolved.

## Regression and evidence

The committed regression module calls the actual renderer, Jitter and Declicker. Cases cover callback starvation/recovery at 16/44.1/48 kHz, epoch reset with old queue samples, full contiguous capacity, a full buffer missing Expected, wrapping sequences, duplicates/late/far/reordered packets, packet bursts, channel replication and I16/U16 conversion, healthy transparency, upstream fade preservation and prerecorded speech.

440 Hz acceptance is phase-aware: capacity output is compared to the correct sequence's exact adjacent sample derivative; healthy resampling is compared sample-for-sample; recovery is checked at sixteen phases against the frequency-derived natural bound `2*A*sin(pi*f/output_rate)` plus the 3 ms envelope derivative. It does not declare every large adjacent difference a pop. Speech tests verify healthy PCM unchanged, exact seam continuity, unchanged PCM after the transition and a residual bound that accounts for natural speech derivatives.

The prerecorded fixture is an unmodified real spoken digit from [Free Spoken Digit Dataset](https://github.com/Jakobovski/free-spoken-digit-dataset), pinned to a source commit. Its original 8 kHz PCM is explicitly interpolated to 16 kHz in the test. Source, SHA-256 and CC BY-SA 4.0 attribution are in `crates/mic-audio/tests/fixtures/README.md`; comparison speech WAVs are adaptations under that same audio license. This short fixture is not conversational intelligibility or subjective speech quality acceptance.

Export evidence from the repository root:

```powershell
$env:MIC_CONTINUITY_ARTIFACT_DIR = Join-Path (Get-Location) 'artifacts/v063-phase1'
cargo test -p mic-audio export_continuity_comparisons --locked -- --nocapture
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/continuity-waveforms.ps1
```

The test-only exporter writes before/after WAVs, comparison JSON and buffer CSVs. Before callback behavior reproduces the baseline callback loop using the equivalent original scalar resampler; the capacity reference is the Phase 0 reproduced seq0->seq9 skip, not a hardware recording. After data uses the actual CPAL renderer/Jitter code. The chart script uses standard .NET Charting offscreen: signed samples, matched full-scale axes, a seam marker at 0 ms, -4..+6 ms around it. All generated evidence stays in ignored artifacts, and export file I/O is confined to the test, never the real-time renderer.

| Scenario | Before seam delta, normalized | After seam delta | Interpretation |
|---|---:|---:|---|
| Callback 440 Hz recovery | -0.300000 | 0 | Starts from last zero output, reaches normal signal over 3 ms. |
| Explicit resampler/epoch reset, 440 Hz | -0.295769 | 0 | Interpolation history is invalidated; transition begins at last output. |
| Full capacity, 440 Hz | -0.398529 (-13059 S16 units) | -0.039764 (-1303 units) | After is the exact normal derivative at this phase, not a zeroed or suppressed seam. |
| Prerecorded speech reset | +0.548691 | 0 | Local transition is continuous; normal PCM resumes after 3 ms. |

Buffer records: before push9 jumped Expected1->9, occupancy8->1 and dropped8. After push9 keeps Expected1, occupancy8, contiguous8 and drops1 incoming packet. CSV before rows describe the baseline proven in Phase 0; after rows are acquired directly from the new Jitter. Plots/WAVs/CSV describe deterministic software scenarios, not actual network latency, device latency, a new ESP32 trace or perceived loudness. Instantaneous seam delta0 is not a claim that every derivative or audible artifact is zero.

For this delivery, the Phase 0 original source copy and current Jitter were additionally compiled together in the ignored `artifacts/v063-phase1/baseline-audit` package and fed the identical packet sequence (0..11, one tick after 0..2). The acquired before/after CSVs extend through packet11; both exported capacity WAV PCM payloads matched those compiled implementations byte-for-byte. `baseline-test.txt` records this independent check. Re-running the standard exporter produces the documented baseline reference rows and the current first capacity-decision trace.

Executed validation on 2026-10-10 (Asia/Taipei):

* Rust Workspace: 66 tests passed (including the optional exporter test, which runs only with the artifact variable); original protocol and telemetry tests remain passing.
* Clippy with `--workspace --all-targets --locked -- -D warnings`, Debug build, formatting and diff checks passed.
* Release build uses `--target-dir target/continuity-validation`: the existing `target/release/mic-server.exe` was locked by the user's running service and was preserved.
* Angular build and all10 existing frontend checks passed, without functional Dashboard changes.
* Existing API/UDP/CPAL integration34 checks and telemetry/history/WebSocket integration39 checks passed on new Debug binaries. Test launchers now support MIC_TEST_PROFILE overrides so the selected new binary is explicit.
* Diagnostic integration passed API guards, event logs/WebSocket, disabled diagnostics with continuing reception, manual stop and actual30-second timeout.
* CPAL output-path integration passed with the new renderer and valid nonempty16k mono capture. This checks software flow and compatibility, not speaker acoustics.

Local implementation commits: `e182fe1` (renderer, admission policy, regression tests and speech fixture), `5fc5846` (waveform plot tool and integration binary selection). The newly built Release service is `target/continuity-validation/release/mic-server.exe`; the existing running Release service was not stopped or replaced.

The v0.6.2 diagnostic capture tap remains before the resampler and final smoother. It intentionally does not capture the final CPAL transition; use the exported renderer evidence or Windows loopback for that stage. No ESP-IDF build was required because firmware was unchanged; no device was flashed.

## Awaiting real A/B

Use baseline `493fc8f` and the Phase 1 binary with identical TX, jitter target, output format, gain and test scenario. Preserve event JSONL, pre-resampler WAV/sidecar and Windows loopback when comparing output seams. Test stable Wi-Fi, delayed/batched packets, output reset/reselection and background CPU load. Correlate capacity reasons by semantics rather than comparing only raw BUFFER_RESYNC totals. Record drop counts and actual callback events.

Network jitter/loss, independent-clock drift, Tokio Burst/rebase behavior, startup holes, playback backlog policy, and Windows mixer/driver/device faults are still outside this fix. No NetEQ, PLC, Opus or adaptive rate control was introduced. Stop after implementation and await real ESP32/Windows A/B; do not treat synthetic traces or CPAL opening successfully as listening approval.
