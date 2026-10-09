# Telemetry v0.5：API 與計算契約

Rust 是唯一監測資料來源。ESP32 wire protocol、音訊格式、配對、音訊回呼沒有改動。新增 `mic-telemetry`，每秒採樣，僅保存記憶體中的最近15分鐘，Core重啟清空。

## 固定界限與執行緒隔離

* 每台TX：900筆snapshot，4096筆事件，兩者也依15分鐘UTC期限移除。事件極大量時先淘汰最舊事件，即使尚未滿15分鐘；避免集合無上限。
* 裝置上限64，沿用v0.4 registry。
* Network actor依目前實際串流擁有者，將音訊收到／tick／callback atomic counters差值歸屬到各裝置。START前設新baseline，STOP／DISCONNECT前結清，停止後不再歸屬播放starvation。累積計數是每台TX自Core啟動起的總量，可跨Session保留。
* Actor透過latest-value `watch` 發佈最多64台的輸入，事件使用1024-capacity `try_send` queue；不取得history store lock、不序列化history。採樣與history讀寫在另一個Tokio task。
* WebSocket使用128-capacity broadcast ring。慢client不回壓Audio Engine；lag時發送 `resync_required`，寫入超過2秒則關閉該連線。
* Event queue溢出的數量公開於 `dropped_event_inputs`（Core整體累計）。一般串流控制流量不會達到上限；它不是無限保留的audit log。

## 時間與區間

所有 `timestamp` 和 `generated_at` 為 **UTC Unix epoch milliseconds**（u64）。圖表axis為UTC，tooltip及事件提示使用帶Z的ISO8601。區間長度用monotonic clock；不以瀏覽器時間推算封包計數。

每秒從相鄰兩筆計數取checked subtraction：

```
received_delta = current.received - previous.received
lost_delta = current.lost - previous.lost
receive_bitrate = bytes_delta * 8 / (interval_ms / 1000)
packet_loss_rate = 100 * lost_delta / (received_delta + lost_delta)
underruns_per_second = underruns_delta / (interval_ms / 1000)
```

bitrate單位bits/s，Angular轉kbps=bits/s÷1000。Underruns Delta為該區間次數；圖表畫的是normalized次／秒，不把delta當成固定剛好1.000秒。

第一筆、Session更換、任一counter減少、採樣間隔不在500–1500ms、輸入超過1500ms未更新、或同區間曾START/STOP（activity epoch改變），都不生成區間數值，使用null並給 `discontinuity`。下筆恢復新baseline。`not_streaming` 時receive/loss/depth/underrun/overrun均為null；cumulative仍保留。STREAMING無封包且有有效區間時bitrate=0是實際零接收，loss denominator=0時loss rate仍為null。

最近60秒遺失率不是各秒百分比的平均：取timestamp落在 `(now-60s, now]` 的有效snapshot，合計lost／(received+lost)。排除null／不連續區間；混合多個Session時只合計各Session內有效區間，不跨Session作減法。API summary與Angular採用相同公式。

loss仍沿用v0.4 playout deadline缺包定義，並非物理Wi-Fi丟包率。Underrun仍合計jitter缺包與playback callback starvation事件，不是丟失樣本數。

## History API

```
GET /api/v1/telemetry/history?device_id=sim-01&range=60s
```

必填：`device_id`（1–64個ASCII英數／`-_.`，且為已發現device），`range`=`60s|5m|15m`。拒絕缺漏、重複、未知query欄位及非法值；400統一 `{error:{code:400,message:"..."}}`，合法但未知裝置404。同v0.4 localhost Host／Origin限制。

Response：

```json
{
  "stream_id": "random-core-boot-id",
  "device_id": "sim-01",
  "range": "60s",
  "generated_at": 1791580000000,
  "snapshots": [],
  "events": [],
  "packet_loss_rate_60s": null,
  "dropped_event_inputs": 0
}
```

已發現但尚未完成第一次採樣回200與空陣列。Snapshots/events各依 `(timestamp,sequence)` 排序，時間區間是 `[generated_at-range, generated_at]`，並受容量與15分鐘限制。

Snapshot fields：

| 欄位 | 型別／意義 |
|---|---|
| id／sequence | `stream_id:sequence`穩定字串／Core內單調全域u64 |
| timestamp | UTC Unix ms |
| device_id／session_id | 字串／nullable Session ID（不是Session secret） |
| connection_state | OFFLINE／DISCOVERED／PAIRING／CONNECTED／STREAMING |
| streaming_state | boolean |
| interval_ms | nullable u64，相鄰monotonic sample間隔 |
| received_packets_delta／lost_packets_delta／out_of_order_packets_delta | nullable u64 |
| receive_bitrate／packet_loss_rate | nullable f64，bits/s及% |
| jitter_buffer_depth | nullable包數，採樣瞬間狀態 |
| buffer_underruns_delta／buffer_overruns_delta | nullable u64，區間計數 |
| buffer_underruns_per_second | nullable f64，normalized次／秒 |
| cumulative | received_packets, lost_packets, out_of_order_packets, received_bytes, buffer_underruns, buffer_overruns，各為u64 |
| discontinuity | null或 baseline, not_streaming, session_changed, stream_transition, counter_reset, sampling_gap, stale_input |

Event fields：`id,sequence,timestamp,device_id,session_id,event_type`；type至少 START_STREAM、STOP_STREAM、DISCONNECT、RECONNECT、SESSION_CHANGED。START/STOP以TX ACK為準；DISCONNECT包含明確中斷及timeout；RECONNECT是已配對device再次完成配對；SESSION_CHANGED是接受新的Session（初次配對也會記錄）。

## WebSocket（沿用 `/api/v1/events`）

保留v0.4每500ms的 `{devices,metrics,active_device}` 狀態訊息；新增discriminated messages：

```json
{"kind":"telemetry_snapshot","stream_id":"boot","snapshot":{"id":"boot:42","sequence":42}}
{"kind":"telemetry_event","stream_id":"boot","event":{"id":"boot:43","event_type":"STOP_STREAM"}}
{"kind":"resync_required"}
```

範例snapshot/event省略其餘已列schema欄位。每個snapshot只在產生時broadcast一次，不在500msstatus timer重複推送；subscribe不是history replay。連線建立後先保持WS接收，再GET history並依id merge，處理REST/WS競爭；重連或lag同樣重取history。Core重啟stream_id改變，清除舊boot資料。

## Angular

Chart.js 4.5.1＋annotation plugin 3.1.0，由Angular component lifecycle建立／更新／destroy。四圖：每秒loss %、bits/s轉kbps、depth包數、underruns次／秒。Range controls為60秒／5分鐘／15分鐘；device切換立即清除其他device畫面；所有事件線有UTC／type／device／session的pointer提示。

前端重同步取15分鐘完整history，供所有範圍和60秒aggregate共同使用；每台選取裝置的前端快取最多900 samples和4096 events。圖表依range篩選。`null`不補零、`spanGaps=false`，discontinuity形成斷線。沒有任何示範fake data。

[Chart.js line gaps](https://www.chartjs.org/docs/latest/charts/line.html)、[tooltips](https://www.chartjs.org/docs/latest/configuration/tooltip.html)、[annotation lines](https://www.chartjs.org/chartjs-plugin-annotation/latest/guide/types/line.html)為圖表官方參考。
