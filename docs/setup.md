# 從零設定

## Windows Rust

1. 從 [Rust 官方](https://www.rust-lang.org/tools/install) 安裝 rustup stable `x86_64-pc-windows-msvc`。
2. 安裝 Visual Studio Build Tools 的 Desktop development with C++（MSVC toolchain 和 Windows SDK）。重新開終端。
3. `rustc --version`、`cargo --version`。若 rustup 已安裝但 PATH 沒更新，當次 PowerShell 用 `$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"`。
4. 進入 Repository `rx`，`cargo build --workspace`。不用另裝虛擬音訊驅動。
5. Windows 設定選一個一般播放裝置。Rust 預設開啟 default output；GUI 可以選同一列舉清單內的其他裝置。USB／藍牙使用 OS 已安裝的一般驅動，是否可用仍取決於該硬體。

本次工具：Rust/Cargo 1.99.0、Windows MSVC，CPAL 0.15.3；依賴已固定在 Cargo.lock。Windows timer request 在程式內成對建立／釋放，不改動永久 OS 設定。

## Angular

安裝 [Node.js](https://nodejs.org/) 24，專案使用 Angular 22。Global ng 非必要：`npm.cmd ci` 後 `npm.cmd run build` 用 local CLI。

Production 靜態 UI：`web/angular/dist/mic-ui/browser`，由 Rust 提供。**從 repo root 啟動 mic-server**，相對路徑才會找到前端。

開發模式：兩個終端分別 `cargo run -p mic-server` 和 `cd web/angular; npm.cmd start`，開 <http://localhost:4200>。前端開發時連至 127.0.0.1:8080；production 使用同源 URL。若改 HTTP port，production UI 自動跟隨；開發 UI 的 8080 必須同步修改 app.ts。

## ESP-IDF／ESP32

安裝 [Espressif 官方 ESP-IDF Windows 工具](https://docs.espressif.com/projects/esp-idf/en/stable/esp32/get-started/windows-setup.html)，本專案以 ESP-IDF 5.x API 撰寫。使用安裝器提供的 **ESP-IDF terminal**，確認 `idf.py --version` 和 `$env:IDF_PATH`。

```powershell
cd firmware/esp32-tx
idf.py set-target esp32
idf.py menuconfig
# Wireless microphone PoC → 本機 SSID／password／device name／UDP ports
idf.py build
idf.py -p COMx flash monitor
```

替換 COMx 為已確認的 ESP32 serial port；不可僅看到 USB serial 就假設是 ESP32。這次環境可見 COM3 USB serial，但 VID/PID 未能證實為 ESP32，未燒錄。

Wi-Fi 凭證只寫入 local `sdkconfig`（已忽略），不要寫入 `sdkconfig.defaults`、原始碼或 Git。`menuconfig` 中為本機 Wi-Fi 設定，不需連接 INMP441 或任何實體麥克風。Wi-Fi STA 關閉 power saving，生成 tick 使用1000 Hz FreeRTOS tick。

Rust 和 ESP32 必須在同一可互通 IPv4 LAN。避開 guest isolation、AP client isolation、阻擋 broadcast 的網段／VPN。Windows 防火牆若阻擋入站，允許本服務 exe 在**私人**網路的 UDP discovery/control/audio，預設41000–41002；不要把 HTTP API 改成公網監聽。本次沒有改動 Windows 防火牆或隱私設定。

Boot log 顯示 device ID 和 32 位 code；在 localhost UI 核准，按「啟動串流」。裝置重新開機會換 code，需重新配對；Wi-Fi 暫斷則保留 boot secret，可自動驗證重連。

## 設定

| 變數 | 預設／意義 |
|---|---|
| MIC_HTTP_PORT | 8080，僅127.0.0.1 |
| MIC_DISCOVERY_PORT | 41000 |
| MIC_CONTROL_PORT | 41001，Rust 接收端 |
| MIC_AUDIO_PORT | 41002 |
| MIC_JITTER_FRAMES | 3，每包10ms，clamp至2–4 |

例如 `$env:MIC_HTTP_PORT='8081'`，再啟動服務。Simulator `--discovery 127.0.0.1:41000`、`--bind 127.0.0.1:0`，也可用 LAN broadcast endpoint；`--id` 可開多台；`--loss-every N` 每 N 包丟一包；`--reorder-every N` 暫存每 N 包直到下一包；`--jitter-ms N` 加0–N ms隨機 delay（queue上限16）；`--reconnect-after N` 開始後N秒重建nonce，模擬一次斷線。

`--code` 僅用於固定 automated test fixture；正常使用讓程序生成 random code。

## C host test

需 GCC 和 Windows SDK-compatible headers/libraries（本機 MSYS2 UCRT64）。`tests/host/run.ps1` 使用 vendored cJSON 1.7.19（附 MIT license），以 Windows BCrypt adapter 代用 mbedTLS SHA/HMAC。這只測 byte encoding、canonical 控制簽章及音源，不會模擬 FreeRTOS／Wi-Fi。
