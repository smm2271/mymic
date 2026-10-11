# Audio Lab v1 驗證紀錄（2026-10-11，Asia/Taipei）

## 修改檔案與設計範圍

| 範圍 | 檔案 |
|---|---|
| Protocol／TX | `crates/mic-protocol/src/{lib,lab}.rs`；`firmware/esp32-tx/main/{CMakeLists.txt,main.c,packet.h,protocol.c,audio_source.c,audio_source.h}`；獨立C/Rust/Python vectors與host runner |
| Audio | `crates/mic-audio/src/{lib,declick,capture,diagnostics,fec,lab_tests}.rs`、`crates/mic-audio/tests/audio_lab.rs`；有效樣本數一路傳遞，SRC ratio／Callback策略不變 |
| Control／Simulator／API | `crates/mic-network/src/{lib,lab_control_tests}.rs`、`crates/mic-server/src/main.rs`、`crates/mic-simulator/src/{main,timing,transmitter}.rs`、`tests/audio-lab-integration.py` |
| Web | `web/angular/src/app/{app.ts,app.html,audio-lab.ts,audio-lab.html,audio-lab-model.ts}`、`web/angular/package.json`、`tests/audio-lab-model.mjs` |
| 文件 | `protocol.md`、`telemetry.md`、`setup.md`、`limitations.md`；新增本紀錄、`audio-lab.md`、`audio-lab-benchmark.md`、`fec-design.md`、`transport-configuration.md` |

共43個檔案；沒有修改sdkconfig／Wi-Fi憑證、沒有加入資料庫／SRC套件依賴，沒有push或部署。Commit按protocol/TX、audio、transport、web、docs分組。

## 已執行

| 指令／驗證 | 結果及界限 |
|---|---|
| `cargo test --workspace --locked --target-dir target/drift-validation` | 完整workspace 90通過、4既有ignored；包含A–E、兩fixture、60秒虛擬case、On/Off checksum。其後新增5ms Capture測試單獨通過（總計91個已執行測試）；最後metadata/collector修改另重跑相關network/audio/capture測試 |
| `cargo clippy --workspace --all-targets --locked --target-dir target/drift-validation -- -D warnings` | 通過 |
| `cargo build --workspace --locked --target-dir target/drift-validation` | Windows MSVC debug build通過；不宣稱本次另跑release build |
| `cargo fmt --all -- --check`、`git diff --check` | 通過 |
| `tests/audio-lab-integration.py` | 真實localhost UDP/Simulator/guarded API，A–E加回復10ms、FEC實際repair與parity increments、target/capacity、STOP/reSTART通過；artifact含before/after與FEC counters |
| `tests/integration.py`，`MIC_TEST_PROFILE=drift-validation/debug` | 原34項配對、拒絕/驗證、Source/Gain/Mute、v1 audio、device輸出開啟/切換、reconnect、UI關閉、timeout、WS回歸通過 |
| `tests/telemetry-integration.py`，同profile | 原39項history/WS同步、每台counter/rate isolation、Session/stop/reconnect等回歸通過 |
| `tests/diagnostics-integration.py`，同profile | Diagnostics enable/disable、事件/JSONL、Session/clock/capture correlation、實際wall-clock30秒auto-stop（headless）通過 |
| `powershell -NoProfile -ExecutionPolicy Bypass -File tests/host/run.ps1` | C/Rust/Python舊v1與Lab v2 5/10ms/parity/control canonical vectors通過，使用host BCrypt crypto adapter，不是Wi-Fi測試 |
| `powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-firmware.ps1` | ESP-IDF **5.5.5**、esp32完整build通過，binary `0xc4030` bytes、app partition剩23%；只build，沒有flash |
| `web/angular: npm.cmd run build`、`npm.cmd test` | Angular production build、原10項history model checks與新Lab counter-window/reset/quantile/network-load model checks通過；沒有宣稱已跑自動瀏覽器點擊驗收 |

初次workspace檔案測試因Windows預設TEMP存取被拒而失敗；只把当次TEMP/TMP設為`artifacts/test-temp`重跑後通過。沒有放寬測試期待值。新版5ms Capture專項測試另抓到writer仍寫固定320 bytes：已修正為寫有效sample count×2，原160-sample writer行為保留；測試證明兩個80-sample frame不夾padding且sidecar offset為80。新增FEC final-empty-deadline測試亦證明最後補償槽不會因Jitter anchor清空而重新修復入列。

## 尚未驗證／已知限制

* ESP32裝置辨識、燒錄、Wi-Fi各case至少60秒、Lab30分鐘soak、CPU/heap/queue壓力、TX實際5ms發送精度未完成。
* 未整合I²S實體麥克風，保留合成音源；語音fixture只在RX虛擬測試。
* Windows CPAL能enumerate/open/switch，不等於實際輸出連續性已確認。沒有loopback／聲學聽測、USB／藍牙驗收或可信端到端latency。
* 4個ignored沿用原Stage A/A.5長測試。其中A.5零漂移自癒contract是已知未滿足的恢復條件，本次沒有將其改成通過；不新增SRC drift補償、不修滿載缺口循環。
* FEC_RECOVERED代表可還原且未逾期，不保證Jitter admission或可聽輸出；FEC不能修復任意loss或clock drift。
* UI記錄最多20場、每場15分鐘，間隔分布為快照採樣；完整packet/callback統計需JSONL。Browser quota/crash可失去未導出的場次，沒有引入SQLite。
* 設定切換刻意stop/refill，不是無縫切換；ACK timeout需要重新確認設定，不能以最後requested值播放。

下一步依 [Benchmark](audio-lab-benchmark.md)在相同硬體/網路跑A–E，保留Serial、Lab JSON、30秒WAV與Diagnostics，再用loopback驗證Windows實際輸出。獨立評估長期滿載缺口恢復與SRC補償，不以FEC改善數字作為漂移根因證據。
