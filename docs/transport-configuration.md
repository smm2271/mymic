# Audio Lab 設定協商與切換

Discovery 能力 `audio_lab_v1` 僅用於是否嘗試協商，不能取代 authenticated ACK。舊裝置繼續 v1 10 ms，Lab更新安全拒絕，不默默降級。

`PUT /api/v1/devices/{id}/transport` 接受完整設定：

```json
{"frame_duration_ms":5,"fec":{"enabled":true,"scheme":"xor","data_shards":4,"parity_shards":1},"jitter_buffer_target_ms":40}
```

frame只能5/10，target只能20/30/40/50/60/80/100，scheme只有xor且固定4+1。Rust拒絕未知欄位與非整數；C拒絕非整數、錯誤範圍及scheme/shards/bool。遵守既有 token、Origin、配對與同時單TX控制限制。不同裝置播放時不能套用本地 RX target。

Actor 將請求設為 Requested，序列送 STOP_STREAM → SET_TRANSPORT →（原先播放時）START_STREAM。每項用原 request-id ACK/retry/cache；SET只允許stopped；ACK必須完整echo已簽署 config＋transport_stream。設定值或token不相符便Failed。Target雖屬RX本地，也放入echo/canonical作為這次 Lab 設定快照的一部分，TX不拿它來延後封包。

SET ACK 後保存 confirmed config；START使用自身request_id作新stream token（u32含0／wrap）。TX在STOP前與sender同步、清generation queue，START新generation epoch重設source index/seq/parity。RX只在START ACK後切换format/tick interval，清Jitter/FEC/Declick/Ring、建立新diagnostic context並預填充。舊v2 stream token、舊Session或frame size被拒收；START ACK前先到的包可能被丟棄，不播放不確定格式。控制操作不在CPAL callback。

失敗保留最後已驗證 config（若SET已ACK就是新值），不宣稱TX實際仍用舊值；ACK timeout時TX是否已套用可能未知。停止或fail-safe停止RX；禁止直接START failed/uncertain狀態，要求先重新STOP/SET成功，或明確選Baseline重新套用。沒有自動無聲降級、沒有在失敗後混用新舊frame。既有5秒API timeout及bounded重試保留；極端多項重試可先HTTP timeout，需查看WS Requested/Applied/Failed，不能把HTTP timeout視為可任意並行再送。

`GET /api/v1/lab` 與既有WS status snapshot新增 lab map、global fec counters、counter_monotonic_us；lab map包含requested/applied/status/error/current transport stream。設定API成功返回confirmed config，它來自matched TX ACK；UI以後續WS applied state展示。

Firmware使用5 ms `esp_timer` task-dispatch callback，callback只通知generation task；task採`esp_timer_get_time()` absolute deadlines，逾期跳過排程機會，不busy wait、不發過期catch-up burst；每個真正生成frame才遞增seq和source samples。這不是I²S生產時鐘，無法保證實際Wi-Fi間隔精確5 ms。timer dispatch可被排程延遲，需對照Serial send完成間隔／queue delay等資料。[ESP-IDF 5.5 官方 ESP Timer 文件](https://docs.espressif.com/projects/esp-idf/en/v5.5/esp32/api-reference/system/esp_timer.html)。

Simulator在Lab模式同樣按產生包累計sequence/source samples，既有v1 baseline的skip-slot模擬維持原行為；TX_TIMING_STATS已認證，發送速率在FEC開啟時包含parity，generated_samples仍只算PCM。ESP32 TX實際間隔目前只有Serial，UI不捏造網路回報。
