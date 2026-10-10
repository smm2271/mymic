# 架構與模組邊界

資料流：ESP32／Rust Simulator → 368-byte UDP PCM → 身分驗證／Session → 有限 jitter buffer → 16 kHz mono ring → 連續線性重取樣 → CPAL WASAPI → Windows 播放端點。

Angular 透過 localhost REST 發出控制，WebSocket 每 500 ms 取得裝置／統計 snapshot。Rust 自行運作，頁面關閉只會釋放該 WebSocket。服務也直接提供 production Angular 靜態資源。

## Rust

* `mic-protocol`：Little Endian byte encoder/decoder；binary authentication；與 JSON 排序無關的控制簽章；Sine、Sweep、Pulse、Silence。
* `mic-device`：OFFLINE／DISCOVERED／PAIRING／CONNECTED／STREAMING，提供可測試合法轉換規則。Network actor 根據已確認事件管理實際狀態，尚未回 ACK 的設定不先標示成功。
* `mic-network`：單一 Tokio actor 同時接收三個 UDP sockets、命令 channel、10 ms playout timer 與維護 timer。Device registry 上限 64；一個裝置至多一個 pending command batch。最多同時一個音訊串流，多個裝置可探索／配對。START 預先保留播放擁有權，防止同時請求競爭。
* `mic-audio`：jitter 最多 8 包；目標 prebuffer 為 2–4 包。預設 overflow 策略保留靠近 expected 的八包，替換最遠或拒絕 incoming；legacy isolation option 才清空積壓。三個連續空 deadline 後重新 prebuffer，避免時鐘停頓造成永久靜音。loss 用 playout deadline 缺包計算，不當成無線鏈路物理丟包率。
* 播放 sample queue：`crossbeam_queue::ArrayQueue`，最多 1600 個 mono samples（100 ms）；積壓超過 960 samples 時清除舊資料防止延遲累加。CPAL callback 不分配、不使用 mutex、不做網路或 HTTP；只從佇列取样、重取樣、複製到裝置聲道並更新 atomic counters。裝置錯誤通知回呼另更新輸出錯誤狀態。
* 專屬 playback thread 保有 CPAL Stream，用 bounded command channel 切換裝置；API 使用 `spawn_blocking` 等待開啟結果。
* gain/mute 在 TX 軟體音源實施，接收核心送出已驗證命令，ACK 後才更新顯示。不是第二次疊加增益。
* Windows 10 ms timer 使用成對 `timeBeginPeriod(1)`／`timeEndPeriod(1)` request；Windows 排程仍不具硬即時保證。
* Continuity Diagnostics 延用同一 RX monotonic origin、bounded producer queues 和背景 JSONL worker。Packet、jitter、ring、callback 與 capture 可依 Session/stream/clock 關聯，離線分析保留證據及未知項，詳見 [continuity-timeline.md](continuity-timeline.md)。

重取樣保留跨 callback 的相位，以實際 Windows default output config sample rate 為準，可用 48 kHz、44.1 kHz 或其他率。支援 F32、I16、U16 的 mono→多聲道複製；輸出裝置以當次列舉索引選擇。線性重取樣不是高品質帶限 resampler。

## Firmware

五個 FreeRTOS tasks：Discovery（1 s）、Control（UDP）、Generation（`vTaskDelayUntil` 每 10 ms）、Transport（4-frame bounded queue）、Telemetry/heartbeat（1 s）。生成時鐘 16 kHz／160 samples，每包遞增 sequence，timestamp 為第一個樣本的樣本序號。Queue 滿則移除最舊 frame；start/stop/reconnect 使用 epoch 拒收舊 frame。

`audio_source.h` 是與 transport 分離的來源介面，可增加 I²S 實作；本次只用軟體音源。控制設定由 mutex 保護，transport 拿快照後編碼；不傳送 C struct 的記憶體表示。Wi-Fi STA/DHCP，斷線重新連線，取得 IP 後更新 discovery nonce、重新驗證 Session；關閉 Wi-Fi power saving。

## 取消與資源

Rust Core 有 oneshot cancellation，結束時回覆 pending requests、盡力送 STOP、釋放 sockets／jitter／ring 並關閉 playback thread。HTTP Ctrl+C 通知所有 WebSocket 結束，再等待服務與 actor 回收。WebSocket 寫入有 2 秒 timeout、單 frame/message 上限 4096 bytes。

配對憑證只在記憶體保留；服務或裝置重開需重新核准。Wi-Fi 暫斷且同一啟動期 secret 未改變時，透過新 nonce+session 自動驗證，還原音源、增益、靜音後恢復先前串流意圖。明確「中斷連線」清除核准；拒絕的 nonce 不再自動列為待配對，重開裝置／重建 nonce 才再出現。

## 官方參考

* [CPAL 0.15.3](https://docs.rs/cpal/0.15.3/cpal/)：裝置配置及 output callback。
* [ESP-IDF Wi-Fi API](https://docs.espressif.com/projects/esp-idf/en/latest/esp32/api-reference/network/esp_wifi.html)：STA、事件與 power saving。
* [Microsoft timeBeginPeriod](https://learn.microsoft.com/en-us/windows/win32/api/timeapi/nf-timeapi-timebeginperiod)：timer request 必須成對釋放。


## v0.5監測隔離

新增 `mic-telemetry`，Core按actual streaming owner歸屬counters，latest watch與bounded event queue送到獨立1秒sampler；API history lock只在sampler/API task使用，Audio Engine不等歷史查詢。每device snapshot ring900／event ring4096，TTL15分鐘；WS broadcast ring128及write timeout，不回壓actor。細節见 [telemetry.md](telemetry.md)。
