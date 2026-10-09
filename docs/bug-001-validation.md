# v0.5.2 validation record — 2026-10-10

Windows 11 build 26200; MSVC Rust Release; output Realtek headphones, 48 kHz stereo. Test credentials are loopback fixtures.

| Check | Result |
|---|---|
| cargo fmt --check | Pass |
| cargo clippy --workspace --all-targets -- -D warnings | Pass |
| cargo test --workspace | 34 tests passed |
| cargo build --release --workspace | Pass |
| Angular production | Pass, 451.71 kB initial bundle |
| Angular history model | 9 checks passed |
| C/Rust protocol golden vectors | Pass |
| v0.4 integration, current debug and Release | 34 checks passed |
| Telemetry integration | 39 checks passed, including signed TX rates, history ranges, reconnect/dedup, UI closure, sessions and device isolation |
| Default timing, 60 s hidden redirected | 6001 sent/received, no gaps/skips/send errors |
| Injected loss every 20 slots, 60 s | 6001 generated, 300 intentionally dropped, 5701 sent/received, 300 observed gaps |
| Actual playback pipeline, 600 s hidden redirected | 99.985 RX packets/s, 16 operational/raw callback underruns; not zero-interruption acceptance |
| System playback loopback, 10 s | 440 Hz, no quiet 10 ms blocks; shared mixer, not human listening |
| Foreground/minimized/switching and human listening | Pending Manual Verification |

See [investigation](bug-001-windows-background-timing.md) for comparison limitations. Re-run results may differ under other desktop loads or hardware. `tests/bug-001-report.py` assembles raw measurement files into `artifacts/bug-001-results.json`; the generated artifacts are intentionally outside version control.
