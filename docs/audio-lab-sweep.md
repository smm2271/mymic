# Audio Lab v2 parameter sweeps

Select a connected `audio_lab_v1` device in Audio Lab. In Parameter Sweep select frame durations, FEC and jitter targets, then generate the matrix. The default 5/10 ms × Off/On × 20/30/50 ms matrix contains 12 candidates. Uncheck any combination to exclude it; recheck to restore. Names and per-combination duration overrides are editable. Save/load the plan before starting if desired.

Defaults: 60 seconds measurement, 5 seconds warmup, one repetition, Automatic, Fixed. Backend accepts measurement/override 10–180 seconds, warmup 0–60 seconds and 1–5 repetitions. Randomized order uses the saved seed and stores the actual `(combination_id, repetition_index)` order. Step-by-step waits after each run; Continue Next advances. Cancel preserves completed runs and marks the current run INTERRUPTED. Manual v1 experiments remain available outside a suite.

The Rust server owns the runner; closing the browser does not stop it. States are IDLE, PREPARING, APPLYING_CONFIG, VERIFYING_CONFIG, WARMING_UP, MEASURING, FINALIZING, WAITING_FOR_USER, COMPLETED, FAILED and CANCELLED. It reuses the signed STOP_STREAM → SET_TRANSPORT → START_STREAM transaction, whose ACK validates configuration and transport stream. It additionally waits for the applied snapshot and a new diagnostic stream. Warmup data is excluded. Measurement uses the existing RX counter_monotonic_us clock. Tokio polling only schedules observations; browser timers never determine the measurement interval.

During a suite other mutation APIs return 409, including device/audio/output/diagnostic changes. Concurrent starts also return 409. Connection, session, diagnostic stream, applied configuration, clock, diagnostic enable changes/drops and counter continuity are checked every 50 ms against network snapshots. A detected fault marks the run INVALID with a structured code. Low audio quality alone does not invalidate a run. Final duration is the observed RX interval, which may overshoot the target by a snapshot/poll interval. A separate watchdog fails stalled RX observations; it does not supply the measurement duration.

## Storage and recovery

`MIC_LAB_DIR` defaults to `data/audio-lab`. `plan.json` stores the latest saved plan. `suite.json` is written via a temporary file and rename at run boundaries; `suite-<timestamp>.json` archives each suite. Completed runs persist before proceeding. Historical suites can be selected in the UI and exported. On startup an unfinished current run is recorded INTERRUPTED with RX_RESTART and the suite becomes FAILED. Execution is never resumed automatically. Filesystem failures fail the suite and surface an error. Archives are not automatically pruned. Storage is local to the RX host; this is not a multi-process scheduler.

## API and schema

All endpoints are under `/api/v1/lab/sweep` and retain existing origin/token checks. Mutation requests require `X-Mic-Token`.

- GET root: current suite and runs; POST root: validated Plan and start.
- POST `/preview`: `{frame:[5,10],fec:[false,true],jitter:[20,30,50]}` → deduplicated Combination array with unsupported reasons.
- GET/PUT `/plan`: load/save validated Plan.
- POST `/cancel`, `/continue`: runner controls.
- GET `/history`: suite IDs; GET `/history/{id}`: archived suite.
- GET `/export`: complete schema version 2 JSON; `?format=csv`: comparison CSV.

Existing WebSocket snapshots now contain `sweep` with the current persisted model, state, current_run, remaining_seconds, estimated_remaining_seconds, runs and comparison_summary. Transport wire protocol and firmware capability are unchanged. Candidates are checked with existing TransportConfig validation; execution requires negotiated audio_lab_v1 capability, then verified signed ACKs. A device rejection is reported rather than treated as a successful measurement.

Schema version 2 preserves plan, requested/effective configuration, TX reported capabilities/build metadata, RX commit, session/stream/clock identity, start/end counters and monotonic timestamps, delta, derived metrics, validity/reasons, diagnostic artifact references and actual execution order. UTC timestamps are Unix milliseconds. TX version remains unavailable if the device does not report build metadata. CSV columns include all five aggregation statistics per metric. CSV blank values and JSON null mean unavailable.

## Metric definitions

All rates use the observed duration `(end_counter_monotonic_us-start_counter_monotonic_us)/1e6` in seconds. Authenticated audio PPS = authenticated_audio_packets delta / duration. Underruns/s, late drops/s, resyncs/s and callback starvations/s use buffer_underruns, late_packet_drops, BUFFER_RESYNC and callback_starvations deltas respectively. FEC recovery count is fec_recovered delta (packets). Missing audio samples ratio = jitter_missing_samples_fed delta / delivered_samples delta; zero denominator yields null. It describes jitter-fed missing samples relative to delivered samples, not end-to-end audio quality or Wi-Fi loss.

Comparison includes only VALID runs, grouped by configuration ID, with mean, median, min, max and population standard deviation. Default ascending underruns/s focuses on continuity. Metric sorting is descriptive and does not establish a universally best setting. `lost_packets` is the existing receiver sequence/playout accounting, not measured Wi-Fi loss. Jitter target is a setting, not measured end-to-end latency; the latter is N/A.

## Validation

Run `cargo test -p mic-server`, `cargo test -p mic-network lab_control_tests`, `npm.cmd test --prefix web/angular`, and `npm.cmd run build --prefix web/angular`. Build simulator/server with `cargo build -p mic-server -p mic-simulator`, then run `python tests/sweep-integration.py` for real localhost UDP negotiation, backend interval, step mode, concurrent/mutation guards, cancellation, archives, exports and persisted results after server restart. Core matrix, counters, statistics and validity unit tests do not sleep; the integration test observes real UDP/server behavior.

Physical ESP32 validation is still required for firmware ACK behavior under actual Wi-Fi loss, all frame/FEC/target combinations, repeated switching, disconnect/reconnect and audible output. Local simulator results are not hardware validation. Polling cannot prove absence of an external transient shorter than network snapshot cadence; the API lock prevents local silent changes, and stream changes retain diagnostic identity. Browser manual experiments are not moved into backend storage by this feature.
