# 音訊連續性時間軸與證據分析

Stage A 已延伸此診斷，新增 source/tick/epoch/feed 觀測、長期摘要與 detail 輪替；目前保存規則及新分析指令見 [drift-stage-a.md](drift-stage-a.md)。下方64 MiB程序停止寫入說明為 `4349c1b` 歷史基準。

## Phase 0 審查結果

本次檢查 firmware `main.c` / `audio_source.c`、RX network actor 的驗證及 START/STOP/timeout、SequenceTracker、Jitter、Engine、Playback、Renderer/Resampler、Diagnostics/Capture、既有單元／整合測試及 continuity/architecture/protocol/testing 文件。現有事件、clock_id、session_id、stream_id、JSONL writer、capture API 和 WAV sidecar 均延用。

* TX 現有來源是 sine/sweep/pulse/silence 合成訊號，**不是 I²S 麥克風擷取**。每 10 ms 產生 160 samples，sequence 在產生時遞增；FreeRTOS queue 滿時丟最舊幀，transport 獨立 drain，sendto 成功只證明本機提交。既有 TX stage timing/window/drop counters 已可用，這次不重複加入。
* RX socket 接收後先檢查 peer、HMAC、session、Streaming，再交給 Engine。新 arrival timestamp 在 socket recv_from 完成後、decode 前取值，**不是 kernel/NIC 到達時間**。不驗證的封包仍由既有 rejected/authentication/session counters 記錄，不將其混入 authenticated PCM 統計。
* SequenceTracker 使用 40 ms 重排序寬限、256 序號記憶；gap 必須有後續較高序號才能確定。尾端整段停止沒有後續序號，不能聲稱知道遺失封包總數。gap/reordered/duplicate 與 jitter playout deficit 是不同母體。
* Jitter 容量八包，既有 prefill target 2–4。預設新 capacity policy 保留最近可播放資料、替換最遠或拒絕較遠；`jitter_capacity_*` 與 `forward_sequence_rejected` 沿用 BUFFER_RESYNC 名稱，**不代表游標 reset**。三次空 deadline 與 receiver >20 ms overdue 才有不同 reset 路徑。舊 v0.6.2 的容量清空說明只適用 legacy isolation option。
* Jitter 缺包輸出零 PCM，再經 de-click，Playback feed 到 1600 個 16 kHz samples 的 ring；既有 >960 samples backlog clear 沒有改動。CPAL 以 device rate 插值、fade、格式轉換及複製 channel；320-input-sample prefill、recovery batch latch/epoch 與所有 isolation switches 保持原行為。
* 既有 diagnostics 已收集所有七類事件及 callback delay budget，但只記異常，缺正常 packet/dequeue/callback 個別觀測，無法還原事件前因。原有 WAV tap 在 de-click 後、ring/resampler 前，不能量測 callback 插入的資料或 Windows 實際聲音。

以上是程式碼觀察；未確認本次聽到斷音的根因，也未另外修改音訊行為。

## JSONL 觀測

沿用 `<RX working directory>/Diagnostics/events-*.jsonl`，可用絕對路徑 `MIC_DIAGNOSTICS_DIR` 覆寫。舊錯誤事件的 schema、API recent/event counts 保留。新增行具有 `record_type`，共用 `clock_id`、`rx_monotonic_us`、`session_id`、`device_id`、`stream_id`。跨執行緒 drain 的行序不一定是時間順序，分析時需排序。

| record_type | 觀測／單位 |
|---|---|
| udp_receive | 每個 authenticated active-session packet 的 sequence、socket completion RX us、前次 arrival interval us、既有 tracker 累計 finalized sequence_gaps、尚在重排序窗口中的 pending_sequence_holes、out_of_order、duplicate。第一包 interval=null；包含被 jitter 拒絕的 authenticated packets。 |
| sequence_status | 每個 Engine tick 寬限 finalize 後的 gap 累計，保留 idle RX 下的 gap 完成時間；不是零 PCM 輸出統計。 |
| jitter_enqueue | decision 完成時間、原 sequence、expected、admitted、occupancy_before/occupancy（packets）、contiguous_depth、capacity=8、late_packet_count/underrun_count。拒絕也記錄；理由見同序號異常事件。 |
| jitter_dequeue | tick decision 完成時間、決策前 expected、前後 occupancy、delivered、started、capacity、late/underrun 累計。包含 prefill tick；prefill 未出列不能当成丟包。 |
| playback_feed | 插入後 ring_after（16 kHz mono samples）、capacity=1600；只在 output opened 時發生。 |
| audio_callback | 入口 RX us、interval、previous budget、requested/supplied device-rate frames、consumed/missing 16 kHz input samples、inserted/starvation/prefill/inactive device frames、rate、ring_before/after。started 是入口 active 且 resampler 已 primed 的 cadence 判定條件，與既有 delay monitor 相同。完成 render 後入 queue，時間是入口。 |
| stream_start / stream_reset | START ACK 的新 context；既有 reset，不增設 session 架構。 |
| diagnostic_health | worker 約每 100 ms 記錄 enabled、enabled_changes、累計 event/observation drops、log error。切換次數可揭露兩個 health snapshot 間短暫關閉。健康訊號不足即不宣稱觀測完整。 |

`supplied_frames` 是沒有缺少 resampler input 的 device frames，包含來源本身為零的正常音訊；不是非零 samples 的數量。`inserted_frames` 是 renderer 沒有 raw output 的 frames，fade 可以讓這些 frames 非零；legacy recovery-off 有補 input zero 的 frame，supplied deficit 與 inserted count 可能不同。ring_before/after 與 callback jitter atomics 是不同執行緒的近似觀察，不是假造原子全系統 snapshot。callback 沒有 sequence 所有權，不能精確說某個 device sample 屬於哪包。

RT producers 只建立固定大小 scalar Observation，再 try-push 到預先配置的 ArrayQueue（4096 records）。不新增鎖、字串配置、序列化或 I/O 到 data callback，也不逐 sample 發送 trace。原事件獨立保留 2048 records，觀測 queue 滿不排擠錯誤 queue；兩者各自回報 drop count。worker 有固定 drain 上限，負責 JSON serialization/context resolution/I/O；diagnostics disabled 關掉音訊 observation 和事件 producers，worker health 仍記錄關閉狀態。

為容納正常 timeline，**每次程序的 JSONL 上限由約 8 MiB 改成約 64 MiB**；最多 overshoot 一行。不是無限錄製，達上限/磁碟失敗後 API 和 capture sidecar 的 event_log_error 明示不完整，記憶體事件繼續。長時間實測每個 scenario 重新啟動測試程序，保留之前檔案。程序突然結束可能遺失 queue 尾端，檔案沒有 shutdown completeness 保證。預設約 100 packet/s、100 tick/s 加 device callbacks；磁碟及 CPU contention 仍應在實機用 on/off 比較，不能由非阻塞設計宣稱零效能成本。

## 離線分析

```powershell
python -X utf8 scripts/analyze-continuity.py Diagnostics/events-EXAMPLE.jsonl --capture Diagnostics/capture-EXAMPLE.json --output artifacts/continuity-report.jsonl
```

省略 `--capture` 時逐 clock/session/stream 分組，窗口為該組第一筆到最後一筆。提供 sidecar 時 summary 限於 capture 的 inclusive start/end，事件分析仍使用同 stream 前 500 ms 的原始觀測。sidecar 保留原 frame sample_offset 與 RX timestamps，新增 observation_drop_count_start/end；WAV 與 event log 的路徑／Session/clock 關聯延用。請在 capture 完成後多等一秒再分析，保留原檔。解析損壞／部分 JSONL 或找不到 matching session/window 時明確失敗。

輸出 `session_summary`、每個原始錯誤的 `event_timeline`、每個 BUFFER_UNDERRUN / AUDIO_CALLBACK_STARVATION / AUDIO_CALLBACK_DELAY 的 `root_cause_candidates`。不合併／抽樣錯誤。Summary 記錄時間窗口、母體、樣本數、nearest-rank P50/P95/P99/min/max、packet reordered/duplicates/finalized gap delta、post-dequeue occupancy、成功序號配對的 jitter residence、callback interval/ring depth、requested/supplied/inserted frames、health 與 capture 診斷狀態。

沒有母體資料時 `collected=false`、percentile=null，無 callback 時 frame totals=null；已觀測的零值才為零。event_counts 缺 key 只表示沒有 admitted event records，不能保證無事件；需同查 health/drop/error。interval 以結束的 arrival/callback 歸屬窗口，可跨窗口開始；residence 以成功 dequeue 歸屬窗口，不將被拒/替換/缺失包納入。

每個候選附 timestamp/sequence/具體 fact、earliest_observed_evidence、window、UDP arrivals/normal、sequence gap delta、pending holes 最大值、expected 在窗口內是否收到、其後 500 ms 同序號 arrival/late 的 follow-up、jitter 是否持續下降、resync/recovery、missing_observations。pending holes 可能因亂序而被填補，不當作已確認 loss；follow-up 明確在事件後，不混入最早前因排序。分類可同時存在，最早觀測不等於因果證明：

* **Network Starvation**：有 health coverage 下 arrival >15 ms、事件距最近 arrival >15 ms 或 tracker gap 增加。僅表示 authenticated RX admission 輸入不足；不能區分 TX generation/queue/send、Wi-Fi、Windows socket、receiver actor 排程。15 ms 是分析閾值，沒有更改音訊節奏。
* **Jitter Buffer Starvation**：deadline expected 缺失，並有 dequeue 觀測。即使 occupancy 非零仍可能只有 future packets，不能由水位 alone 推論可播放。
* **Audio Callback Starvation**：active callback cadence 超過上一 buffer duration +20 ms，有 callback 資料；僅識別 invocation cadence 缺口，不是推測 WASAPI 聲音中斷。
* **Playback Ring Starvation**：實際 callback 的 resampler input 空取樣，有 missing_input_samples/inserted frames/ring 證據。正常 callback cadence 下也可能發生，不能單憑事件名稱改叫排程延遲。
* **Scheduling Delay**：callback 實際 cadence gap 或 receiver_scheduler_stall >20 ms 的實際 tick 檢測。OS/DPC/thread 細節尚無觀測。
* **Unknown / Insufficient Evidence**：缺 health、關閉／drops／log error、缺正常 packet/dequeue/callback，或沒有可支持 trigger 的數據。已識別層仍可與 Unknown 共存，表示前因或另一層資料不足。

還無法由此診斷 Windows mixer/driver/DAC/acoustic 中斷；若用戶聽到問題卻沒有 pipeline deficit，要加 Windows loopback／ETW 與聽到的時間標記。要判斷實際 wire loss、TX vs 網路 delay，仍需 serial TX window/queue/send evidence 與同步網路封包觀測；不可直接相減 ESP32/RX 時鐘。

## 驗證與下一次 ESP32 + Windows 操作

1. 在 rx 工作目錄 build 新 binary：`cargo build --workspace --locked --target-dir target/continuity-diagnosis`。使用新 `target/continuity-diagnosis/debug/mic-server.exe`，設定獨立 MIC_HTTP_PORT/MIC_DISCOVERY_PORT/MIC_CONTROL_PORT/MIC_AUDIO_PORT（真 ESP32 要與其設定一致）。避免与其他 Core port 衝突，不終止無關既有程序。設定 `MIC_DIAGNOSTICS_DIR` 到這次 scenario 的絕對目錄。firmware 本次未修改、不需要為此 patch flash。
2. 保留 ESP32 Serial logs（TX window IDs、report age、queue drops/depth、send errors、stage timing）。Dashboard 明確 pair，選 output、sine/440 Hz、固定 gain/mute/jitter target，START。先穩定播放一秒，避免測試事件前史不足。
3. Dashboard Start Capture 保留既有最多 30 秒／480000 samples WAV；在症狀出現時記錄人聽到的本機時間，另錄 Windows loopback。Capture 完成後保存 WAV、sidecar、對應 event JSONL、serial log、binary hash、輸出 rate/channels、scenario 與聽音記錄。不要用 WAV wall time 差直接映射 sample offset，使用 sidecar frames。
4. 執行上面的 analyzer 指令，查同 Session/stream symptom 附近事件和候選；先查 disabled_changes/drops/log_error 與缺少資料，再評估 earliest evidence。若沒候選而 loopback 有中斷，標記 output/OS 外層待測，不宣稱 Wi-Fi 故障。
5. 同 TX/output 設定依序測 baseline、延遲/遺失/亂序/突發、Windows foreground/background/CPU load、output 重新選取、STOP/START 與 session reconnect，每 scenario 保存獨立檔案。所有 reset/recovery 保留 reason，不只比較事件總數。
6. 用 `MIC_DISABLE_DIAGNOSTICS=1` 或既有 PATCH `/api/v1/diagnostics`，比較 events-off/capture-off、events-on/capture-off、events-on/capture-on。on/off 需要同樣環境；drop/error 不為零即報告 incomplete。原 API 使用 X-Mic-Token。

自動測試使用 actual Jitter/Renderer 測 PCM equivalence、loss/reorder/burst/late、callback starvation/recovery/epoch 等，timeline fixtures 測證據分類／健康資料不足／session isolation，不把 fixture 當實機 trace。既有 diagnostics integration 增加 real local authenticated UDP 的正常資料、30 秒 capture 關聯與 report；output integration 檢查 Windows CPAL callback 的 requested/supplied/consumed/ring 與 capture Session。

## 本次修改與驗證結果（2026-10-10，Asia/Taipei）

| 修改檔案 | 用途 |
|---|---|
| crates/mic-audio/src/diagnostics.rs | fixed-size observation queue、背景 JSONL、health、enabled changes、獨立 drop counters |
| crates/mic-audio/src/lib.rs | UDP/jitter/feed/callback observation hooks，admission 與 on/off PCM equivalence tests |
| crates/mic-audio/src/sequence.rs | 只讀 pending holes getter，不改 tracker 行為 |
| crates/mic-audio/src/output.rs | supplied frames 計數，不改 renderer output |
| crates/mic-audio/src/capture.rs | observation drop start/end sidecar 欄位 |
| crates/mic-network/src/lib.rs | socket completion timestamp 傳入 Engine，不改驗證／session 決策 |
| scripts/analyze-continuity.py | 離線 summary/statistics/timeline/evidence candidates JSONL |
| scripts/verify.ps1 | 加入 evidence analysis tests |
| tests/continuity-analysis-test.py | 14 個 deterministic timeline/evidence tests |
| tests/diagnostics-integration.py | authenticated UDP trace、30 秒 capture/report 關聯 |
| tests/diagnostics-output-integration.py | 真 CPAL callback trace 與 capture Session/frame counters |
| docs/continuity-timeline.md | 本文件、schema/分類／操作／驗證界限 |
| docs/continuity-diagnostics.md | 指向目前診斷 schema，區分歷史 baseline |
| docs/architecture.md | 對齊現有 capacity policy，連結新時間軸文件 |

| 本次實際執行 | 結果 |
|---|---|
| cargo test --workspace --locked --target-dir target/continuity-diagnosis | 74 tests passed（47 audio unit、3 audio integration、1 device、1 network、6 protocol、1 server、3 simulator、12 telemetry） |
| python -X utf8 tests/continuity-analysis-test.py | 14 tests passed |
| cargo clippy --workspace --all-targets --locked --target-dir target/continuity-diagnosis -- -D warnings | passed |
| cargo fmt --all -- --check / git diff --check | passed |
| cargo build --workspace --locked --target-dir target/continuity-diagnosis | passed Debug build；未覆蓋既有程序使用的 binaries |
| MIC_TEST_PROFILE=continuity-diagnosis/debug tests/integration.py | 34 checks passed；真 localhost UDP、Session reconnect、Windows output open/switch |
| 相同 profile tests/telemetry-integration.py | 39 checks passed；既有 history/WebSocket/session isolation |
| 相同 profile tests/diagnostics-integration.py | passed；實際 30 秒 timeout、headless WAV、觀測 JSONL、capture 範圍 summary／evidence；最後產生 694 report records |
| 相同 profile tests/diagnostics-output-integration.py | passed；非空 16 kHz mono S16LE WAV、真 Windows 48 kHz / 2-channel CPAL 消耗，callback trace 與 capture Session 關聯，零 capture/observation drops |

初次 capture unit tests 因既有 TEMP 目錄寫入權限出現三個 PermissionDenied；將 TEMP/TMP 設為工作區 `artifacts/continuity-diagnosis/test-temp` 後全部通過。未改測試以隱藏失敗或改音訊處理。新資料與結果在 ignored artifacts/ / Diagnostics/，不提交 WAV、credentials 或生成 binary。Firmware/frontend 未變更，本次未重新 build firmware/frontend，也未做 ESP32 flashing。

**尚未執行 ESP32 + Windows 聽音、loopback、實際 Wi-Fi 故障或效能 A/B 驗證**。能定位有記錄的軟體 deficit 所在層並列出候選 antecedents；無法證明所有聽到的斷音原因。若無資料，report 明示缺少 packet、dequeue、callback、health 或外部 output 觀測，不把穩定補幀輸出當成 UDP 零遺失。
