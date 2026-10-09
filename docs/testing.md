# 驗證紀錄

日期：2026-10-10，Asia/Taipei。執行於 Windows，本機已有 MSVC／GCC、Rust stable、Node.js、Python；未找到 ESP-IDF。以下只列實際執行結果。

## 建置／靜態檢查

| 檢查 | 結果 |
|---|---|
| `cargo build --workspace` | 通過，含Windows CPAL/WASAPI及Simulator |
| `cargo test --workspace` | 13項unit tests通過、0失敗 |
| `cargo fmt --all -- --check` | 通過 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 通過 |
| `web/angular: npm.cmd run build` | production建置通過，initial bundle约184.8 kB |
| `tests/host/run.ps1` | GCC `-Wall -Wextra -Werror`通過，C golden vectors通過 |
| ESP-IDF target esp32 build | **未執行：未安裝／未提供idf.py與IDF_PATH** |

## Unit tests：13

1. Audio encode/decode roundtrip、錯誤HMAC／截斷封包拒絕。
2. Control JSON roundtrip、正確／錯誤key驗證。
3. u32 sequence wraparound。
4. Sine／Silence／mute音源切換。
5. C/Rust共用audio/control標準向量，獨立Python標準HMAC生成。
6. Device合法state transitions與非法跳轉。
7. Jitter reorder／loss／duplicate。
8. Jitter overflow長序列容量限制。
9. Jitter wraparound／underrun／prebuffer。
10. 發射端停頓後重新prebuffer，不能永久靜音。
11. 16→48 kHz重取樣：一秒訊號保留约440次正向crossings與振幅限制。
12. 44.1 kHz重取樣：sample consumption與DC连续性。
13. API非法source／未知欄位／超範圍與NaN gain validation。

## Black-box integration：34 checks

`python -X utf8 tests/integration.py`：啟動真實Rust binaries、獨立UDP sockets與localhost HTTP，Node原生WebSocket client。使用18080／41100–41102，測完終止測試processes；公開固定code僅為test fixture。注入每13包丟一包、每7包亂序、0–4 ms jitter，以及16秒後fresh nonce重連。

實際通過：

* Headless service啟動、CPAL output enumeration。
* 無API token mutation拒絕、惡意HTTP/WS Origin拒絕、WS snapshot送达／client關閉。
* Gain／source／未知欄位驗證、缺失device HTTP404。
* 真UDP discovery、錯誤credential拒絕、正確credential配對。
* CONNECTED／STREAMING區分、START/STOP ACK。
* Source／gain／mute逐項ACK、state顯示已確認設定。
* 丟包／亂序統計、jitter storage上限。
* Windows default output stream開啟、切換output ID。
* 無瀏覽器時串流繼續。
* Fresh nonce自動重新驗證、還原sine／gain0.25／mute=false、恢復串流。
* STOP後不繼續傳audio、明確disconnect清除核准。
* 模擬無回覆TX，配對 bounded retries後HTTP504、恢復DISCOVERED。
* User rejection、同nonce再次broadcast仍保持OFFLINE。

完整當次結果保存於 `artifacts/integration-result.json`（忽略Git）；最後故障注入測試觀察到received1514、lost145、out-of-order564、buffer_overruns2。這些為人工注入故障與本機排程影響下的功能驗證，**不是實機效能或50ms延遲達標證據**。重新執行數字可不同。

## C host cross-language

編譯韌體實際 `protocol.c`、`packet.h`、`audio_source.c` 與cJSON 1.7.19；Windows BCrypt SHA/HMAC adapter代替ESP-IDF mbedTLS。

逐byte比對368-byte PCM、完整SET_GAIN canonical簽章、header decode／invalid length／version拒絕、控制tag篡改拒絕、Sine振幅上限、Silence／Mute。這不是ESP-IDF Wi-Fi/RTOS build或實機測試。

## Chrome UI

實際瀏覽 <http://127.0.0.1:8080>，繁體中文畫面使用真服務／Simulator `ui-test`：

* 配對credential輸入→CONNECTED。
* 啟動串流→STREAMING，packet counters透過WebSocket增加。
* 音源改為頻率掃描，靜音後按鈕顯示「取消靜音」，state由ACK同步。
* Windows列表含BenQ、Realtek喇叭、Realtek耳機、HP顯示音訊；default為Realtek耳機。
* 重啟核心後頁面重新連線可恢復；idle時underrun保持0。
* 截圖 `artifacts/dashboard.png`。CSS提供700px breakpoint與2-column mobile stats；未另做手機實機驗收。

## 必須另做的驗收

**沒有已確認的ESP32實機驗證、聲音聽測、loopback capture、端到端延遲量測、四小時soak。** Windows stream已成功開啟和切換，不將其描述為已聲學聽到440Hz。

實機連上同LAN後依README配對、播放四種音源、改gain/mute/output、關閉UI、關閉／恢復Wi-Fi並確認聲音恢復。

四小時監測（已手動核准且啟動串流）：

```powershell
python -X utf8 tests/soak.py --hours 4
```

每5秒將devices、metrics與UTC+8時刻寫入 `artifacts/soak.jsonl`，驗證有STREAMING device且received持續增加；它不取代聽測，也不證明四小時沒有聲學glitch。任何failed observation回nonzero，需釐清網路、音訊、source狀態。

延遲定義：起點是TX generator產生指定首樣本，終點為speaker/headphone聲學輸出對應波形的錄音時刻。需同時的reference trigger與獨立audio recording／儀器，涵蓋Wi-Fi、jitter、SRC、WASAPI及硬體。若只量sender monotonic→receiver arrival，應報network latency，不能稱end-to-end；本次没有可校準測量。


## v0.5驗證（2026-10-10，Asia/Taipei）

* Rust `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings`通過。
* `cargo test --workspace --locked`：**23項通過**（原13項＋Telemetry9項＋Core/history隔離1項）。Telemetry測1秒cadence、900筆上限、15分鐘TTL、Session與counter reset、採樣gap、停止／秒內stop-start、range filtering、device isolation、event capacity/TTL、weighted60秒loss、slow subscriber lag。
* 隔離測試持有history寫鎖，真實UDP配對／發包，Audio Engine仍接收至少110／120包；sampling與history阻塞不回壓音訊actor。
* Rust production：`cargo build --workspace --release --locked`通過。
* Angular production：`npm.cmd run build --prefix web/angular`通過，initial bundle约449.1kB，低於500kB budget。
* Angular model：`npm.cmd test --prefix web/angular` **9项檢查通過**，用實際history-model.ts驗證refresh restore、REST/live競爭、reconnect dedup、device切換、boot更換、900 capacity、weighted loss與event dedup；fixtures只存在tests，不用於dashboard。
* v0.4 `tests/integration.py` **34項回歸檢查通過**，原配對／串流／播放／重連仍正常。
* `tests/telemetry-integration.py` 使用Release binaries，**37項檢查通過**：非法query統一400、unknown404、真1秒樣本、idle nullable、TX isolation、真WS重連／HTTP history去重、無UI採樣、串流不中斷、新nonce/newSession/reconnect events、stop nullable、A/B擁有權切換、三種range及時間排序。結果 `artifacts/telemetry-integration-result.json`。
* C/Rust golden tests通過；`git diff -- firmware crates/mic-protocol`為空，ESP32音訊封包未改動。

Chrome手動驗證實際服務資料：

* 4張真實曲線，每11包丟1包的Simulator fixture产生约9% loss；不是前端fake data。
* refresh前123筆，refresh後130筆，從Core恢復；切到未串流B時累積received0，rate/depth顯示無資料。
* 60秒與5分鐘時間軸切換，UTC數值tooltip、STOP_STREAM線提示type/device/session，停止後不假造0速率。
* 390×844 viewport，圖表單欄；document clientWidth=375、scrollWidth=375，沒有horizontal overflow；已還原viewport override。
* Browser error logs為空。截图 `artifacts/telemetry-dashboard.png`、`artifacts/telemetry-mobile.png`；結果 `artifacts/telemetry-browser-result.json`。

v0.5未新增硬體／聲學／延遲實測；900筆/TTL透過可控制時間的unit tests驗證，不宣稱已跑15分鐘或4小時Wi-Fi實機soak。事件ring每台4096固定上限，極高事件量會較早淘汰最舊事件，詳細見telemetry契約。

可重跑全部：`powershell -ExecutionPolicy Bypass -File scripts/verify.ps1`，需要cargo、Node.js24、Python、GCC與可用Windowsoutput。整合程序在finally結束；本次瀏覽器測試程序也會清理，不留下背景服務。
