# Audio continuity diagnostics v0.6.2

## Phase 0: code inventory (before changes)

* ESP32 generation uses `vTaskDelayUntil(10 ms)` and a bounded FreeRTOS frame queue. Queue overflow removes the oldest frame. Sequence increments at generation, not UDP delivery. The original `generated_us` precedes PCM generation; the original queue-delay measurement also includes generation and transport's context snapshot. The original send-duration measurement includes encoding and HMAC, not just `sendto`.
* Transport drains the queue independently. Backlog can produce closely spaced submissions. Successful `sendto` is local submission, not delivery evidence.
* TX timing windows advance only in transport. The telemetry task previously read the same published structure every second, without a window ID or synchronization. Identical reports can therefore be repeated stale windows; identical fresh windows are also possible. None of this establishes the cause of audible gaps.
* RX authenticates/session-checks UDP before admission. SequenceTracker distinguishes bounded reordered gaps, while Jitter rejects stale packets, has an eight-packet capacity, and advances one expected sequence per 10 ms tick. Missing expected packets yield zero PCM. Three empty misses reset its cursor. Capacity overflow, large forward sequence jumps and a receiver scheduler stall also rebuffer.
* De-click runs after Jitter and before the playback ring. CPAL consumes a 16 kHz mono ring through linear interpolation to the actual device rate and replicates channels. Callback counters existed, but lacked individual event timestamps and packet context. Existing telemetry sampled underrun events every tenth occurrence and omitted late/recovery events.
* Angular uses the existing 500 ms WebSocket snapshots plus one-second historical telemetry. These are suitable for bounded diagnostics snapshots without extra polling.

These are code observations, not an established root cause. Hardware/network/output measurements remain necessary.

## RX event definitions

All new continuity events and capture frame positions use microseconds since the single `Diagnostics` instance's `Instant` origin. `clock_id` identifies that origin across logs and metadata; it changes after a Core restart. Unix milliseconds in capture metadata are human reference times only. Existing historical telemetry retains its UTC API timestamps. ESP32 `esp_timer_get_time()` has a different origin: **do not subtract TX and RX clocks**.

`stream_id` is a Core-local ordinal incremented after each acknowledged START_STREAM, even if the authenticated session stays the same. `device_id` and authenticated `session_id` are resolved outside the callback. The bounded registry retains the latest 128 stream contexts; older pending records can retain the stream ordinal but lack strings after extremely rapid restarts.

`packet_sequence` is the original wrapping u32 packet sequence. `expected_sequence` is the jitter playout cursor at the decision, not the next UDP packet prediction. Sequences reset with TX stream epochs; compare them within a stream/session and use modular ordering. Callback events have null sequences because the ring stores samples without packet ownership.

| Event / reason | Observation |
|---|---|
| BUFFER_UNDERRUN / playout_deadline_missing | Expected packet absent at an operational jitter tick. Includes its expected sequence. Muted deficits use a separate reason; startup prefill produces no event. This is not proof of UDP loss. |
| LATE_PACKET / behind_playout_cursor | Authenticated, nonduplicate packet rejected because its sequence is behind the cursor. RX can establish missed playout timing, not whether TX scheduling or network transport caused it. |
| BUFFER_RESYNC / jitter_capacity, forward_sequence_jump | Existing capacity/jump reset, with pre-reset packet depth and cursor. |
| BUFFER_RESYNC / three_empty_deadlines | Existing reset after three consecutive missing deadlines with no queued packets. |
| BUFFER_RESYNC / receiver_scheduler_stall | Existing >20 ms receiver catch-up reset. Normal START/STOP/session resets do not fabricate this event. |
| BUFFER_RESYNC / playback_ring_backlog_clear | Existing playback ring backlog clear; pre-clear sample count. |
| AUDIO_RECOVERY / jitter_packet_delivery_resumed | First real jitter packet after an interruption/reset, before de-click and physical playback. |
| AUDIO_CALLBACK_STARVATION / resampler_input_empty | An active CPAL callback tried to consume missing 16 kHz input; one event per affected callback. Initial/rebuffering silence is not classified as starvation. |
| AUDIO_RECOVERY / callback_consumption_resumed | First completed nonstarved callback following callback starvation. |
| AUDIO_CALLBACK_DELAY | Observed callback entry gap exceeds the previous requested buffer's duration plus 20 ms. Includes gap and budget. An observed scheduling gap, not a WASAPI/acoustic underrun guarantee. |
| AUDIO_OUTPUT_ERROR | CPAL error callback invoked; inspect existing output_state for its error text. |

Jitter/target depth are packet counts. Callback jitter depth is the latest atomic observation, not a synchronized packet-level snapshot. `ring_depth_samples` is a 16 kHz sample count where directly observable, otherwise null. Jitter reasons and callback reasons identify separate stages. Producers on different threads can enqueue in a different order than their timestamp order; sort by `rx_monotonic_us` for analysis.

The event ArrayQueue has capacity 2048. Producers do not wait, allocate strings, serialize, or perform I/O. Attempted cumulative counts include dropped events; `drop_count` counts full-queue rejections. The worker retains 128 recent records and 60 one-second counting buckets (current second plus 59 previous seconds), containing only events admitted to the queue. Disabled producers stop both counts and events. Pending events may finish draining after disable. UI uses existing 500 ms WebSocket snapshots without extra polling.

The worker lazily writes JSONL under `<Core working directory>/Diagnostics`, with a per-process log limit of approximately 8 MiB. Disk errors/limit are surfaced as `event_log_error`; in-memory diagnostics continue. Old runs/captures are not automatically deleted. `MIC_DIAGNOSTICS_DIR` overrides the directory; use an absolute path. `MIC_DISABLE_DIAGNOSTICS=1` starts with events disabled; PATCH or the Dashboard can enable them again. No playback/jitter capacities were increased and no UDP protocol or pairing fields changed.

## Capture and correlation

Recording is off by default. Dashboard Start/Stop or these authenticated endpoints control it:

* GET `/api/v1/diagnostics`: continuity and capture status.
* PATCH `/api/v1/diagnostics` with `{"enabled":false}`: event producers off (independent of capture).
* POST `/api/v1/diagnostics/capture/start` and `/stop`: manual capture control, using the existing X-Mic-Token guard.

Capture taps `Playback::feed` **after de-click and any existing ring backlog clear, immediately before inserting the 16 kHz mono frame into the playback ring**. Output must be opened; headless mode produces a valid empty WAV, not invented played audio. WAV header is RIFF PCM format 1, 16000 Hz, one channel, 16 bits, little endian, with lengths finalized at stop. Actual output can be 44.1/48 kHz F32/I16/U16; interpolation/channel replication occurs later. No device-format data is mislabeled as 16 kHz PCM.

The capture queue holds 128 fixed 160-sample frames, with recording generation tags isolating restarts. A separate writer handles all disk I/O without holding status locks over disk operations. Limit is 480000 samples and a 30-second monotonic recording interval, including periods when no frames are fed. Finishing status can be delayed by disk latency, but frames outside that interval are excluded. Stop reasons include manual_stop, timeout_30s, queue_overflow_incomplete, stream_changed and disk_error. Overflow stops an incomplete recording rather than silently claiming uninterrupted audio. Files are finalized before completion is published; normal service shutdown requests stop. Abrupt process/power loss can leave an unfinished WAV/header or missing sidecar.

The adjacent `.json` sidecar contains start/end RX microseconds and Unix milliseconds, clock/device/session/stream IDs, sample count, drop counts, stop reason, errors, event-log path, and `frames`: each accepted frame's sample offset and RX timestamp at the tap. To align an event, locate neighboring frame timestamps and use `sample_offset / 16000` to find the corresponding WAV region. Rebuffering intervals can contain no tapped frames: do not map `event_time - capture_start` directly to the WAV sample offset. The sidecar explicitly preserves those time gaps.

Limitations: tap data may subsequently be dropped by the playback ring or rendered differently by the resampler/output conversion. Callback-synthesized zeros, Windows mixing, driver/hardware defects and speaker acoustics are outside this WAV. Events and a separate Windows loopback recording are required to study those stages. Diagnostic I/O is bounded/off the audio thread, but CPU/disk contention can still affect system scheduling; compare enabled/disabled runs. Counters/JSONL can be incomplete after diagnostic queue overflow or disk failure.

## TX timing semantics after correction

Transport owns window accumulation under a dedicated short critical section. Logger copies and consumes a published window under that same lock, then computes/prints outside it. Connection reset is synchronized. A new frame epoch resets interval history, so START/STOP gaps are not counted as active-stream intervals. Reports identify `window_id`, ESP32 `start_us/end_us`, actual duration and rate; missing IDs indicate a logger missed an overwritten window. Logger prints `no fresh window` and age instead of repeating stale measurements. Windows are normally at least one second and publish on the next send attempt, not an independent timer; no frames means no fresh window. This is explicit rather than labeling stale data as fresh one-second statistics.

| Field | Definition |
|---|---|
| sent / rate | Successful full local datagram submissions in that window / actual window duration. No remote delivery guarantee. |
| intervals / mean, min, max, std | Positive intervals between successful send completions; interval count is separate from packet count. Sample std uses N-1. Cross-window adjacent intervals are included in the ending window. Squared accumulation is double to avoid integer overflow after stalls. |
| late | Successful send-completion intervals >15000 µs, not a count of network-late packets or precise RTOS deadline misses. |
| err | Failed/non-full local send submissions in the window. |
| q_delay_max | Dequeue timestamp minus timestamp immediately before the initial nonblocking enqueue attempt. Excludes PCM generation and transport context snapshot; includes queue operation/retry bookkeeping. |
| gen_max_us | Enqueue-attempt timestamp minus PCM generation-start timestamp (including frame setup). |
| prep_max_us | Immediately-before-send timestamp minus transport preparation entry (encoding/HMAC); excludes the context snapshot. |
| send_max | Return from sendto minus immediately before sendto; syscall-only duration. This differs from the previous field which included preparation. |
| queue_depth_max | Sampled queued depth at dequeue plus that dequeued frame. Approximate under concurrent generation, not an exact instantaneous high-water mark. |
| gen_total_at_report / queue_drops_total | Cumulative generation count read at logging time / cumulative queue-overflow drops captured at window publication. Separate from send errors. |
| TX Frame | Last attempted frame in the window: sequence and generation, enqueue attempt, dequeue, send-start and completion times on the ESP32 clock. Not a complete per-frame trace. |

TX's bounded queue can still backlog and transport can drain bursts. This patch observes it; it does not change generation/send scheduling or mask discontinuities. One serial summary per window cannot resolve every sub-window timing excursion.

## Reproduction and interpretation

Start the new Core binary from the `rx` directory, open Dashboard, pair ESP32, select sine and an output, then start the stream. Record a 30-second capture around the audible symptom while preserving its JSONL and metadata. Keep TX Serial logs with window IDs and report ages. Record scenario, Windows output format, Wi-Fi conditions and enabled/disabled diagnostics. Never compare timestamps between hosts without synchronization.

1. BUFFER_UNDERRUN(expected N) followed by LATE_PACKET(packet N) shows late arrival relative to playout. TX/serial submission evidence and network traces are still needed to attribute delay to TX versus Wi-Fi.
2. BUFFER_UNDERRUN without eventual packet N establishes a playout deficit only: TX queue drops, local send errors and network loss are competing causes.
3. BUFFER_RESYNC reason identifies jitter capacity, sequence discontinuity, empty stall, RX scheduler stall or playback backlog; inspect immediately preceding events/depths.
4. AUDIO_CALLBACK_STARVATION with no jitter deficit suggests starvation between jitter feeding and callback consumption; inspect ring depth and receiver scheduler/backlog events. This is a hypothesis to test, not a proven hardware root cause.
5. Callback gap/output error or audible corruption absent from pre-resampler WAV motivates Windows loopback/output-path investigation. A clean pre-resampler WAV alone does not establish a healthy output device.
6. Recovery reasons show logical jitter delivery versus callback consumption; de-click does not guarantee physical click-free sound.

## Automated validation and next hardware acceptance

Rust tests cover normal no-false-events, unique delayed packet/sequence context, missing packet, empty/forward/capacity resync, jitter recovery, callback timing/starvation/recovery, queue overflow, disabled PCM equivalence, timestamp wrap/reorder, capture command lifecycle/restart, WAV header/length/endianness, 30-second sample cap, forced-deadline timeout, stream change, read-only-handle write failure and invalid directory. Deadline manipulation is a unit test, not a real-time/hardware test.

`tests/diagnostics-integration.py` exercises real local UDP authentication/fault injection, API guards, diagnostics disabled while reception continues, existing WebSocket payload, JSONL sink, manual stop and an actual 30-second wall-clock timeout. Its capture is intentionally headless/empty. `tests/diagnostics-output-integration.py` separately opens real Windows CPAL output and verifies that output consumes samples while a nonempty capture and frame sidecar are produced. This tests the software path, not speaker acoustics. Angular tests retain the project's history-model runner; the new panel's template/type bindings are checked by Angular build and its payload by WebSocket integration. Automated browser-click and real Windows acoustic validation remain manual.

Executed on 2026-10-10 (Asia/Taipei):

| Check | Result |
|---|---|
| `cargo test --workspace --locked` | 54 tests passed; no failures. |
| `cargo clippy --workspace --all-targets --locked -- -D warnings` | Passed. |
| `cargo fmt --all -- --check`, `git diff --check` | Passed. |
| `cargo build --workspace --release --locked` | Passed. |
| `cargo build --workspace --locked --target-dir target/continuity-validation` | Passed Debug build. Standard target/debug binaries were locked by the user's already-running processes; those processes were preserved. |
| `npm.cmd run build`, `npm.cmd test` in web/angular | Build passed; all 10 existing frontend history checks passed. New panel template compiled. |
| `MIC_TEST_PROFILE=release python -X utf8 tests/integration.py` | All 34 checks passed, including pairing/reconnect, UDP faults, Windows output selection and playback independence from UI. |
| `python -X utf8 tests/telemetry-integration.py` | All 39 checks passed. |
| `MIC_TEST_PROFILE=continuity-validation/debug python -X utf8 tests/diagnostics-integration.py` | Passed guards, event schema/JSONL, WebSocket, disable/reception, manual capture, real 30-second stop. |
| `python -X utf8 tests/diagnostics-output-integration.py` | Passed: latest run 8800 nonzero-source samples (first run 8160), zero capture drops/errors, 16 kHz mono S16LE WAV, per-frame sidecar; actual output 48 kHz / 2 channels. See artifacts/diagnostics-output-result.json. |
| `powershell -NoProfile -ExecutionPolicy Bypass -File tests/host/run.ps1` | C/Rust golden vectors, invalid headers and synthetic audio safety passed. |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-firmware.ps1` | ESP-IDF 5.5.5 esp32 build passed; binary 0xc3460 bytes, 24% app-partition space free. No flashing. |

The new server features require the newly built binary. Existing running Debug server/simulator instances were not restarted or replaced. Logs/WAV/test results remain under ignored artifacts/ and Diagnostics/ directories; no local Wi-Fi configuration or generated capture was committed. The firmware build helper's installation paths are configurable parameters. The standard verify script includes headless diagnostics integration; the output-path integration is a separate explicitly hardware-dependent command.

For hardware acceptance (not performed by this task):

1. Obtain explicit flashing authorization; use the built firmware only after that approval. This task did not flash or reset ESP32.
2. Collect fresh serial windows for at least 60 seconds, including idle, START/STOP and restart; confirm unique IDs, actual durations/rates, queue drops/depth, stage times and no repeated stale reports.
3. Run 440 Hz for at least five minutes under baseline Wi-Fi and repeat with induced network delay/loss. Preserve 30-second WAV/sidecar/JSONL runs containing the symptom and note audible times.
4. Repeat foreground/background, CPU load, output selection and output disconnect/reconnect; collect Windows loopback if the pre-resampler capture differs from hearing.
5. Compare event-off/capture-off, event-on/capture-off and event-on/capture-on runs, with identical jitter settings. Check diagnostics Drop Count, capture overflow/errors and output counters.
6. Attribute each interruption only after correlating evidence. Record remaining unknowns; do not declare the pop fixed from simulation or a passing build.
