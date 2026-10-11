# Wire protocol v1／API v1

Audio Lab v1 新增**協商後才使用**的音訊 wire v2；未協商裝置仍走下述原 v1。詳見 [設定協商](transport-configuration.md) 與 [FEC](fec-design.md)。

## Lab wire v2（212／372 bytes）

保留前32byte欄位與little-endian語意；offset4 version=2，offset5 type=1 PCM或2 parity，offset14 payload length=160或320。offset32新增u32 transport stream token，offset36開始80或160個i16 PCM，最後16byte為header＋payload HMAC-SHA256前16byte。總長必須精確等於36＋payload＋16，並匹配已確認的frame size；type2只在FEC啟用且group base對齊4時接受。type1 sequence每個真正生成音訊frame遞增，timestamp為首樣本index、每frame加80/160；type2 sequence/timestamp是該四包分組base，不占音訊sequence。START token使舊Session內的舊stream不可混入新timeline。

新增控制欄位`transport`完整設定與`transport_stream:u32`，只在Lab控制／ACK出現。若有transport，原canonical bytes尾端附加：

```
|lab1|frame_duration_ms|fec_enabled_0_or_1|xor|4|1|jitter_buffer_target_ms|transport_stream
```

所有可影響播放／產生的Lab設定與token納入HMAC，不能只簽kind/request。固定scheme/shards由strictvalidation保證；旧消息沒有此suffix，原golden vector unchanged。SET_TRANSPORT只能stopped，ACK完整echo signed values；START帶同一confirmed config及新stream token。

本機新增`PUT /api/v1/devices/{id}/transport`與`GET /api/v1/lab`；WS原snapshot擴充`lab`、`fec`、`counter_monotonic_us`，保留原schema欄位。Lab恢復Baseline是10ms／FECoff／30ms的**已協商v2**設定；真正舊版v1依然可從未協商或重新配對的裝置使用，UI不假裝兩者wire overhead相同。

所有整數使用 Little Endian；控制 JSON UTF-8 最多 2048 bytes；音訊不使用 JSON。所有網路埠可設定。

| 用途 | 預設 |
|---|---|
| TX → Rust DISCOVER broadcast | UDP 41000 |
| Rust 控制／ACK／heartbeat | UDP 41001 |
| Rust 音訊接收 | UDP 41002 |
| ESP32 控制接收 | UDP 41001（不同主機） |
| 本機 API／WebSocket／UI | TCP 127.0.0.1:8080 |

Simulator 使用 ephemeral 本機控制埠，透過 `control_port` 告知 Rust。ESP32 discovery socket 廣播，Rust OFFER 回 TX 公告的 control endpoint。

## 控制 envelope

必要：`version:1`, `kind:string`, `request_id:u32`, `device_id:string`（1–64 bytes）。選用欄位缺省為空／0／null：

`name`（≤96 bytes）、`device_type`、`capabilities:string[]`、`nonce`、`commitment`、`session`、`proof`、`audio_port:u16`、`control_port:u16`、`source`、`gain`、`mute`、`error`。

來源：`sine|sweep|pulse|silence`；gain finite 0–1；mute boolean。實作拒絕未知 JSON 欄位、錯誤版本及錯誤型別。

| kind | 方向／用途 |
|---|---|
| DISCOVER | TX → broadcast，身分描述、32 hex nonce、64 hex commitment、control_port |
| OFFER | Rust → TX，回相同 request/nonce、Rust audio/control ports；不表示核准 |
| PAIR_REQUEST | 使用者輸入正確 code 後 Rust → TX，nonce、全新 32 hex session、ports、code HMAC |
| PAIR_ACCEPT／PAIR_REJECT | TX → Rust，對應 request/session 與 Session HMAC；reject 附 error |
| HEARTBEAT | 已配對雙方每秒送出，Session HMAC，不需 ACK |
| START_STREAM／STOP_STREAM | Rust → TX，Session HMAC，回 ACK／ERROR |
| SET_AUDIO_SOURCE／SET_GAIN／SET_MUTE | Rust → TX，帶設定欄位，Session HMAC，回 ACK／ERROR |
| ACK／ERROR | TX → Rust，對應 request/session，成功或錯誤資訊 |

UI 拒絕未知裝置時，Rust 本機停止接納該 nonce，並盡力送未驗證 PAIR_REJECT 通知；TX 不依此未驗證訊息改變安全狀態。它不是 Session 拒絕簽章或遠端關機授權。

每個控制操作 initial send + 最多 3 retries，每 500 ms 重送**相同** request_id/session/payload；約 2–2.5 s 返回 timeout。設定 batch 順序逐項 ACK，API 至多等 5 s。TX 保留最近 32 個 request/response；retry 回同樣 response，不能再次重啟串流或生成 Session。同一 nonce/session 的 PAIR_REQUEST 重試保留相同 Session。

API batch 非交易：前面的設定成功而後項 timeout 時，成功部分保留，error 明確回報。5 秒 session timeout 標示 OFFLINE；仍核准的裝置用新 discovery nonce 重新驗證。

## 配對與驗證

TX 每次開機從硬體／OS CSPRNG 生成 128-bit secret，顯示 32 位小寫 hex code；**不在網路中傳送 code**。nonce 是另一个 128-bit random hex，每次 Wi-Fi 恢復更新。

```
commitment = hex(SHA256(UTF8(code + "|" + nonce)))
session_key = HMAC-SHA256(UTF8(code), UTF8(nonce + "|" + session))
```

使用者經 Serial monitor／Simulator 終端取得 code 並在 localhost UI 核准。Rust 先檢查 commitment，TX 再驗 PAIR_REQUEST 的 code HMAC，Rust 再驗 Session key 簽署的 PAIR_ACCEPT；Device ID／來源 IP 只是 routing，並非唯一身分證據。Session secret、API token 都只保存在記憶體。

控制簽章固定 canonical bytes（不以 JSON serialization 簽章）：

```
version|kind|request_id|device_id|nonce|session|audio_port|control_port|source|gain_milli|mute
```

缺省 source／gain／mute 為空字串；gain_milli=`round(gain*1000)` 的十進位整數；mute=`1` 或 `0`；u32/u16 使用無前導零十進位。proof=`hex(HMAC-SHA256(key,canonical_bytes))`，完整 32-byte tag。PAIR_REQUEST key 是 UTF-8 code，其餘是 binary session_key。描述性 discovery 欄位和 error 文字不是授權依據；error 文字本身未加入 canonical bytes。

安全範圍是可信 LAN PoC：有配對驗證與 authenticated PCM，**不加密**；沒有完整 DTLS/防 DoS/持久 trust store。最近 32 request 的重試 cache 與 audio 有限 replay window 不能取代完整抗重放協定。不得當成生產安全認證。

## PCM datagram：固定 368 bytes

| offset | bytes | 欄位 |
|---|---|---|
| 0 | 4 | ASCII `WMIC` |
| 4 | 1 | version=1 |
| 5 | 1 | type=1 (PCM) |
| 6 | 4 | sequence u32 LE |
| 10 | 4 | timestamp u32 LE，首個樣本的 source sample index |
| 14 | 2 | payload length=320 (LE 0x0140) |
| 16 | 16 | Session ID，由 session hex decode |
| 32 | 320 | 160 個 i16 LE，16 kHz mono，10 ms |
| 352 | 16 | HMAC-SHA256(session_key, bytes[0..352]) 的前 16 bytes |

保留建議的 16-byte base header，擴充 16-byte Session ID 與尾端 16-byte authentication tag。Decoder 先檢查固定長度、magic、version、type、payload length，再 constant-time 比對 tag；Session 必須相符。

序號以 modulo 2^32 wrap，先後以 signed wrapping distance 比較，使用 window 遠小於 2^31；timestamp 每 frame 加160，也可 wrap。START 重設音訊 timeline；重新配對使用新 Session。包內各欄位明確 byte encode，不能發送 native/padded C struct。

`tests/vectors.json` 是 C/Rust/Python 獨立生成比對的標準向量；`tests/host/golden_test.c` 使用韌體實際 `packet.h`、`protocol.c`。Host crypto adapter 僅代用 Windows BCrypt SHA/HMAC，不代表 ESP-IDF mbedTLS 或 Wi-Fi 已測試。

## 本機 HTTP schema

所有寫入要求 `Content-Type: application/json` 和 `X-Mic-Token`；token 從 GET status 取得，服務重啟會更新。Host 限 localhost/127.0.0.1；Origin 限 service 自己 origin 和 localhost／127.0.0.1:4200。無 Origin 的 native CLI 可用，惡意跨站 Origin 拒絕；WebSocket 套用同一限制。此 token 只防跨站，不是 Windows 本機惡意程式隔離。

| Method／route | request body／response |
|---|---|
| GET `/api/v1/status` | `{service,protocol_version,sample_rate,frame_ms,api_token}` |
| GET `/api/v1/devices` | Device[]：id,name,device_type,capabilities,address,state,source,gain,mute,last_error |
| POST `/api/v1/devices/{id}/pair` | `{code:string,reject?:boolean}`，成功 `{ok:true}`；reject 時 code 可以空字串 |
| POST `/api/v1/devices/{id}/disconnect` | 無 body，成功 `{ok:true}` |
| PATCH `/api/v1/devices/{id}/audio` | `{source?,gain?,mute?,streaming?}`，至少一項，成功 `{ok:true}` |
| GET `/api/v1/audio/outputs` | `[{id:string,name:string,is_default:boolean}]` |
| PUT `/api/v1/audio/output` | `{id:string}`；id=`default` 或列舉索引，成功 `{name:string}` |
| GET `/api/v1/metrics` | Metrics object |
| WS `/api/v1/events` | 每 500 ms `{devices,metrics,active_device}` snapshot |

正常為200；schema/credential錯誤400、Origin/token錯誤403、裝置不存在404、控制競爭／未連線409、控制timeout504。JSON 拒絕／domain錯誤統一 `{error:{code:number,message:string}}`；HTTP parser／WebSocket upgrade 的底層 rejection 可能使用框架格式。Body 上限4096。

Metrics：received_packets、lost_packets、out_of_order_packets、duplicate_packets、receive_bitrate（啟動後累積平均、有效 audio datagram bytes，不含 UDP/IP/Wi-Fi overhead）、jitter_buffer_depth（包數）、buffer_underruns、buffer_overruns、invalid_packets、output_state。Underrun 合計缺包 playout deadlines 和播放 callback starvation 次數，**不是同一單位的樣本數**。統計不能用來聲稱端到端音訊延遲。


## v0.5 Telemetry（不改wire protocol）

新增 `GET /api/v1/telemetry/history?device_id=...&range=60s|5m|15m` 與同WebSocket的 `telemetry_snapshot`／`telemetry_event`／`resync_required` 訊息。既有status snapshot及 `/metrics` 保留為累積legacy統計，不作為每台TX每秒速率。UTC timestamp、nullable值、相鄰差值、事件與query驗證契約見 [telemetry.md](telemetry.md)。
