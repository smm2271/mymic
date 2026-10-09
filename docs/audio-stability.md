# Audio stability and timing (v0.5.2)

PCM remains 16 kHz, signed 16-bit mono, 160 samples per packet, with the existing 368-byte authenticated packet and pairing protocol. No ESP32 packet change or database is required.

## Timing

Windows Simulator defaults to `--timing-mode waitable`: a dedicated transmitter thread uses an absolute monotonic 10 ms timeline, a Windows high-resolution waitable timer and an MMCSS `Audio` registration. Timer and MMCSS handles are released on exit. There is no spin wait. UDP sends are nonblocking. Control and bounded asynchronous JSONL output are separate from the timing thread. STOP closes the shared send gate before its acknowledgement; reconnect/START begins a fresh timeline.

On a missed deadline, the transmitter selects the current slot rather than replaying obsolete audio. Normal sends have at least 5 ms spacing; an overly close slot is skipped. Sequence is the slot number and audio timestamp is slot × 160, including skipped slots. Generator phase follows that sample timestamp. The receiver can therefore observe a sequence gap caused by a documented TX skip; it must not label it proven network loss. Fault-injection queues are capped at 16 packets. Diagnostics queues hold at most 16 reports; slow disk or network output drops diagnostics rather than blocking audio.

`current` compares the former Tokio interval/Skip policy on the new isolated thread (it is not the untouched old executable). `absolute` uses absolute deadlines with ordinary thread sleep. `period` adds the paired RAII `timeBeginPeriod(1)` / `timeEndPeriod(1)` experiment to absolute scheduling. It is an experiment, not a promise of 1 ms wake accuracy. Non-Windows default is `absolute`; Windows dependencies are target-specific.

```powershell
target/release/mic-simulator.exe --id sim-01 --code 0123456789abcdef0123456789abcdef --timing-mode waitable --timing-diagnostics --diagnostics-file artifacts/simulator-timing.jsonl
```

Reports use monotonic interval durations, every second and once at stream stop. They include generated/sent packets, samples, send failures, actual packet/sample rates, send interval and scheduling delay P50/P95/P99/max, missed deadlines, skipped timeline slots, drift, mode and MMCSS status. `missed_deadlines` counts wakes at least one period late; skipped slots are counted separately, including the spacing guard. Raw samples are bounded to 256 per reporting interval for offline percentiles. JSONL files grow for the requested experiment and are not the Core's history storage.

## Metric meanings

| Metric | Meaning |
|---|---|
| TX generated / sent / send_errors | Produced frames, successful local UDP submissions, failed submissions; not remote delivery acknowledgements |
| TX sequence_gaps_generated_by_tx | Timeline slots deliberately skipped due to lateness/spacing |
| TX intentional_loss_drops / queue_drops | Explicit fault injection / bounded test queue overflow |
| RX sequence_gaps | Missing authenticated sequence slots after a 40 ms reorder allowance; cause is not established |
| RX rejected / authentication / session / header counts | Rejected datagrams, tracked before audio delivery |
| RX accepted / delivered / late_packet_drops / jitter_buffer_drops | Distinct jitter admission, playout, stale drops and capacity drops |
| Legacy lost_packets | Missing frame at a jitter playout deadline; retained for API compatibility, not UDP loss |
| Buffer underruns / callback_starvations | Operational unmuted starvation / raw playback callback starvation |
| receive_bitrate | Authenticated PCM payload bits/s, excluding headers and authentication |

Sequence tracking has a bounded 256-slot window; larger discontinuities reset its pending window rather than fabricate a large loss estimate. Counter resets/session changes invalidate interval statistics. Core snapshots use the actual counter acquisition interval for packet rates. Global `/metrics` rejection totals and per-device authenticated audio counters have different scopes.

Playback still has the existing 1600-sample capacity; the fix does not increase jitter capacity. Playback begins/rebuffers after 320 queued samples. The callback performs no logging, network I/O, heap allocation or telemetry serialization. Mono samples are resampled once per output frame and copied to channels. Muted starvation is distinguished from audible underrun; raw starvation remains visible. Headless output does not accumulate samples. RX bounds late clock catch-up and rebases after a stall rather than remaining permanently behind.

## Dashboard and API

Existing history URL, UTC timestamps, stable snapshot IDs, WebSocket events and 900-snapshot/15-minute retention remain compatible. Snapshots add `rx_packet_rate`, optional `tx_packet_rate`, `tx_scheduling_drift_ms`, optional `tx` summary, `loss_basis: "playout_deadline_deficit"` and `bitrate_basis: "pcm_payload"`. Signed optional `TX_TIMING_STATS` control-plane diagnostics are checked against device, peer IP, session HMAC and increasing report sequence; stale reports become null after 2.5 seconds. ESP32 devices without this extension display no TX rate, never invented data.

The four original charts remain, with clearer deficit/payload labels. Two additional charts compare real TX/RX packet rates and TX drift. Browser local time is the default; UTC can be selected. Cumulative counters, interval rates and 60-second statistics remain distinct. UI closure cannot stop the Core sampler.

See [BUG-001 measurements and acceptance](bug-001-windows-background-timing.md) for measured results and pending manual checks.
