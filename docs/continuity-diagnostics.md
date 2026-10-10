# Audio continuity diagnostics v0.6.2

## Phase 0: code inventory (before changes)

* ESP32 generation uses `vTaskDelayUntil(10 ms)` and a bounded FreeRTOS frame queue. Queue overflow removes the oldest frame. Sequence increments at generation, not UDP delivery. The original `generated_us` precedes PCM generation; the original queue-delay measurement also includes generation and transport's context snapshot. The original send-duration measurement includes encoding and HMAC, not just `sendto`.
* Transport drains the queue independently. Backlog can produce closely spaced submissions. Successful `sendto` is local submission, not delivery evidence.
* TX timing windows advance only in transport. The telemetry task previously read the same published structure every second, without a window ID or synchronization. Identical reports can therefore be repeated stale windows; identical fresh windows are also possible. None of this establishes the cause of audible gaps.
* RX authenticates/session-checks UDP before admission. SequenceTracker distinguishes bounded reordered gaps, while Jitter rejects stale packets, has an eight-packet capacity, and advances one expected sequence per 10 ms tick. Missing expected packets yield zero PCM. Three empty misses reset its cursor. Capacity overflow, large forward sequence jumps and a receiver scheduler stall also rebuffer.
* De-click runs after Jitter and before the playback ring. CPAL consumes a 16 kHz mono ring through linear interpolation to the actual device rate and replicates channels. Callback counters existed, but lacked individual event timestamps and packet context. Existing telemetry sampled underrun events every tenth occurrence and omitted late/recovery events.
* Angular uses the existing 500 ms WebSocket snapshots plus one-second historical telemetry. These are suitable for bounded diagnostics snapshots without extra polling.

These are code observations, not an established root cause. Hardware/network/output measurements remain necessary.
