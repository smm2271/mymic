# Audio Lab Benchmark Procedure

## 可重現軟體驗證

以下在`rx`目錄執行；Windows暫存目錄若不可寫，可將TEMP/TMP只在當次測試process設為工作區內`artifacts/test-temp`（不修改音訊核心測試期待值）。

```powershell
New-Item -ItemType Directory -Force artifacts/test-temp | Out-Null
$env:TEMP=(Resolve-Path artifacts/test-temp).Path
$env:TMP=$env:TEMP
cargo fmt --all -- --check
cargo test --workspace --locked --target-dir target/drift-validation
cargo clippy --workspace --all-targets --locked --target-dir target/drift-validation -- -D warnings
cargo build --workspace --locked --target-dir target/drift-validation
python -X utf8 tests/audio-lab-integration.py
powershell -NoProfile -ExecutionPolicy Bypass -File tests/host/run.ps1
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/build-firmware.ps1
cd web/angular
npm.cmd run build
npm.cmd test
```

`lab_tests`直接使用production Engine/Jitter/Declick/Playback queue與Renderer，虛擬TX/RX tick／Callback時鐘；每個case 60 秒，含兩種fixture、四種延遲、On/Off checksum。不是純數學模型，也不是Wi-Fi／WASAPI實機測試。此測試虛擬device rate16k，其他device rate／多聲道由既有Renderer回歸涵蓋；它不證明真實Windows callback排程。

## Windows＋ESP32 實際矩陣（尚未驗證）

| Case | Frame | FEC | Target |
|---|---|---|---|
| A | 10 ms | Off | 30 ms |
| B | 5 ms | Off | 30 ms |
| C | 5 ms | 4+1 | 30 ms |
| D | 5 ms | 4+1 | 40 ms |
| E | 5 ms | 4+1 | 60 ms |

1. 記錄Git commit、RX/firmware binary版本與Wi-Fi環境；以使用者現有sdkconfig建置。確認實際ESP32與COM埠後手動flash/monitor，本任務沒有自動燒錄或重設硬體。
2. 原UI配對，低增益440Hz；啟用原Diagnostics，保存Serial timing log。確認能力與Applied frame/FEC/target及packet rate；任何Failed先停止並重新套用Baseline。
3. 各case至少60秒，切換後先等待預填充與穩定，再開始Lab區間；前30秒保留既有WAV Capture（5ms時6000有效frames、仍最多480000samples），導出metadata/JSONL/manifest/summary及Lab JSON。同頁比較區間差值，不拿全域累積值當場次結果。
4. 用同一套可控fault proxy或Simulator注入單包、連續loss、2/5/10/20ms delay／reorder，再測各case。區分TX主動drop、網路未到、FEC repair、deadline late及callback缺供。FECRecovered須核對Jitter admission與後續delivered，不等於聽到正常聲音。
5. Windows WASAPI loopback或外部可信音訊量測記錄实际输出；對齊脈衝/測試音與capture，但pre-ring WAV的正常節奏不能證明喇叭／USB／藍牙無斷音。量端到端延遲需可信同時基準，不能直接相減TX/RX独立monotonic clocks。
6. 長期每case至少30分鐘，記memory、FEC group上限、Jitter/Ring watermarks、回呼間隔、error/drop counters；用Stage A資料追蹤漂移，不假設FEC處理漂移。切回0／負漂移與滿載缺口仍需獨立實驗。

## 理論負載（非實測）

PCM均為256kbps。舊v1 10ms是368bytes×100pps；Lab v2 10ms372bytes×100pps，5ms212bytes×200pps。4+1加25%packet rate及等長parity，5ms FEC為250pps。IPv4無options UDP/IP overhead每包28bytes：v1 baseline316.8kbps、v2 10ms320kbps、v2 5ms384kbps、v2 5msFEC480kbps；不含Wi-Fi/MAC/PHY重傳及控制封包。縮短frame會提高封包頻率，增加buffer會增加等待；FEC不是所有遺失的解法。

目前軟體測試通過與firmware建置狀態以本次交付／Git紀錄為準；實機矩陣、acoustic聽測、loopback latency未執行，不能把虛擬60秒case算成實際60秒Wi-Fi驗收。
