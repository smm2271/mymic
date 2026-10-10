# Wireless Mic PoC v0.6.2

## v0.6.2 Audio continuity diagnostics

Dashboard now exposes bounded RX continuity events, per-type cumulative/60-second counts, diagnostic drops and optional 30-second WAV capture. Capture is post-de-click 16 kHz mono PCM before the device resampler; callback starvation/gap/output-error events cover later stages. ESP32 Serial timing reports identify fresh windows and separate generation, queue residence, preparation and send duration. See [diagnostic definitions, test evidence and hardware acceptance](docs/continuity-diagnostics.md). Hardware root cause and audible pop resolution remain unverified.

## v0.5.2 Windows audio timing

Simulator now uses an isolated absolute-deadline transmitter with Windows high-resolution waitable timers and MMCSS Audio scheduling. Optional monotonic JSONL diagnostics expose TX timing, intentional skips and send errors. PCM wire format and ESP32 pairing remain unchanged. Dashboard adds real TX/RX packet-rate and drift charts, browser-local/UTC time selection, and distinguishes playout deficits from proven network loss.

Measured hidden-background Release runs sustained approximately 100 packets/s, including ten minutes. Occasional playback callback underruns remain; foreground/minimized/switching and human listening are **Pending Manual Verification**. See [audio stability](docs/audio-stability.md), [BUG-001 evidence and test commands](docs/bug-001-windows-background-timing.md), and the generated `artifacts/bug-001-results.json`.

ESP32-WROOM-32 軟體產生測試音訊，透過 Wi-Fi UDP 傳至 Windows Rust 核心，由一般喇叭／耳機播放。Angular 繁體中文控制台只管理裝置；關閉瀏覽器不會停止音訊。不使用實體麥克風、虛擬音訊裝置、雲端、帳號或資料庫。

專案位於既有 `mymic/rx` Repository，保留原有 `src/main.rs` Hello World；實際接收服務是 **`mic-server`**，不要直接使用不帶 `-p` 的 `cargo run`。

## v0.5 即時監測

Rust每秒採樣，每台TX保存最近15分鐘（900筆）history；新增四張Chart.js折線圖、UTC hover、START/STOP／disconnect/reconnect／Session事件線、60秒／5分鐘／15分鐘與裝置切換。頁面重新整理／WebSocket重連從核心恢復歷史，關閉UI不影響採樣。儀表板主指標為**最近60秒封包遺失率**，累積統計在詳細資訊。停止或不連續區間用無資料，不填假零。

新增API：`GET /api/v1/telemetry/history?device_id=sim-01&range=60s`；完整schema見 [docs/telemetry.md](docs/telemetry.md)。音訊封包／ESP32韌體未改動。

## 快速啟動（Windows PowerShell）

先安裝 Rust stable（MSVC）、Visual Studio C++ Build Tools 與 Node.js 24。詳細從零設定見 [docs/setup.md](docs/setup.md)。

```powershell
cd C:\Users\zalic\desktop\mymic\rx
# 僅當 Rust 未在 PATH：
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
cargo build --workspace
cd web/angular
npm.cmd ci
npm.cmd run build
cd ../..
cargo run -p mic-server
```

開啟 <http://127.0.0.1:8080>。服務會開啟系統預設播放輸出；無可用輸出時仍可管理裝置，畫面會顯示錯誤。

另開終端執行模擬器：

```powershell
cargo run -p mic-simulator
```

1. 複製模擬器終端顯示的 32 位配對憑證，輸入裝置卡片並按「核准配對」。
2. 配對完成後為「已連線」，按「啟動串流」播放 440 Hz 測試訊號。
3. 可切換音源、增益、靜音與 Windows 輸出。關閉網頁後串流繼續。
4. Ctrl+C 停止服務；會取消網路任務並關閉播放串流。正式執行可直接用 `target/debug/mic-server.exe`。

初次播放先降低系統音量。發射訊號振幅上限為 full scale 的 15%，脈衝再減半；這不代表聲壓已量測或保證耳機音量安全。

模擬丟包、亂序、jitter、Wi-Fi 重連：

```powershell
cargo run -p mic-simulator -- --id sim-02 --loss-every 13 --reorder-every 7 --jitter-ms 4 --reconnect-after 20
```

ESP32 安裝與燒錄：在 ESP-IDF 終端進入 `firmware/esp32-tx`，執行 `idf.py set-target esp32`、`idf.py menuconfig` 設定本機 Wi-Fi，再 `idf.py build`、`idf.py -p COMx flash monitor`。讀取 Serial monitor 的配對憑證，在 Web UI 核准後啟動串流。**sdkconfig 含 Wi-Fi 密碼，已忽略，不可提交。**

## 驗證

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --release
node --experimental-strip-types tests/history-model.mjs
python -X utf8 tests/telemetry-integration.py
cargo fmt --all -- --check
python -X utf8 tests/integration.py
powershell -ExecutionPolicy Bypass -File tests/host/run.ps1
```

整合測試會使用 18080／41100–41102，不占用預設服務埠，並開啟真實 Windows 音訊輸出。需要 Node.js、Python、可用播放裝置及已建置的 Rust binaries。C host 測試需 GCC（本機 `C:/msys64/ucrt64/bin/gcc.exe`）。

## 模組與文件

| 位置 | 功能 |
|---|---|
| `crates/mic-protocol` | 版本化 JSON 控制、HMAC、PCM binary、測試訊號 |
| `crates/mic-device` | 裝置狀態及合法轉換 |
| `crates/mic-telemetry` | 每秒採樣、固定容量history、UTC事件、broadcast |
| `crates/mic-network` | UDP、明確核准配對、Session、重試與重連 |
| `crates/mic-audio` | jitter buffer、無鎖播放佇列、重取樣、CPAL/WASAPI |
| `crates/mic-server` | 本機 REST、WebSocket、Angular 靜態資源 |
| `crates/mic-simulator` | 同協定 Software TX、網路故障注入 |
| `firmware/esp32-tx` | ESP-IDF C、FreeRTOS、STA、軟體音源 |
| `web/angular` | 繁體中文響應式控制台 |

[架構](docs/architecture.md) · [協定與 API](docs/protocol.md) · [環境設定](docs/setup.md) · [測試結果](docs/testing.md) · [限制](docs/limitations.md)

目前已驗證 Windows 模擬器端到端流程及 C/Rust 共用向量；**尚未完成 ESP-IDF 完整建置、ESP32 實機、聲音聽測、50 ms 延遲量測及四小時耐久驗收**。詳見測試文件，不將模擬器結果當成實機通過。
