# Stage A：Drift Telemetry 與未補償基準

日期：2026-10-11（Asia/Taipei）。Stage A 已獲批准；Stage B–D 暫緩。本次不增加 ASRC controller、adaptive jitter、PLC 或新的 resync policy。SRC step 仍為16000/device rate；UDP、jitter target、容量、deadline及恢復行為保持原有語意。

## 觀測與時鐘

沿用 `4349c1b` Diagnostics origin與bounded RT queues。新增：

* authenticated UDP／jitter enqueue 的原始 `source_sample_index`（header u32 sample index），不再只靠sequence推算source時間。
* `receiver_tick` 的 `scheduled_us`、`overdue_us`；予定時間由本次RX observation time減Tokio實際overdue換算到共同origin，包含兩次clock reading之間的小量誤差，不是kernel timestamp。既有Burst/20ms判斷不變。
* `playback_feed` 實際成功accepted_samples／push失敗dropped_samples；`playback_clear` 實際discarded_samples、既有ring epoch。
* callback入口/完成 `output_epoch`、`output_epoch_end`，valid input consumed、stale epoch discarded、resampler phase/step只讀值。跨epoch callback標示不可用。
* local `playout_epoch` 純診斷計數：reset、三個空deadline取消anchor、prefill anchor變動及legacy重定錨時遞增。不改wire session／stream、不控制cursor、不改ring既有epoch。`jitter_dequeue.operational`分開標示logical slot，避免prefill tick當source出列。

RT callback仍只組fixed scalar record並try-push，沒有新增鎖／序列化／I/O／控制器。新增清除/丟棄計數可能產生很小的CPU成本；virtual on/off輸出相同不能保證Windows排程成本為零，效能與實機驗收仍需A/B。

`MIC_DRIFT_TELEMETRY=off`關閉新增source/ring/phase/tick擴充與每秒drift摘要；原continuity diagnostics照常。既有`MIC_DISABLE_DIAGNOSTICS=1`／API disabled關閉音訊事件及observation producers，health仍記錄。旗標不修改音訊決策；log retention是共同診斷sink，仍使用新保存規則。

## 長期摘要與保存

背景worker將observations按 `[秒開始, 秒結束)`、clock/session/stream/playout/output epoch分桶，最多64個pending buckets、8秒fault/health資訊；2秒watermark讓跨thread晚到記錄有機會歸入原窗口。已關閉窗口收到更晚資料／bucket overflow會累加discarded_summary_buckets_total，不回寫舊秒、不把漏資料當零。程序突然結束可能少最後約2秒摘要；manifest明示tail可能未完成，沒有shutdown durability保證。

每桶輸出counts及field的count/sum/min/max/mean，母體為**成功進diagnostic queue的observations**。水位平均是observation-weighted，不是時間積分平均。不存在欄位即未蒐集，不用0代替。每秒保留source first/last及以nominal16000Hz算arrival residual最低的unique in-order header point，作lower-envelope slope；這仍不能識別緩慢變化的network/dispatch delay。

所有錯誤類型／reset、missing/duplicate/reordered sequence、callback inactive/prefill/starvation、sample discard、enabled變動、health不足、diagnostic drops/error會在該秒標invalid。health每秒至少5筆的門檻只是一個資料品質條件，不是完整無漏包證明。缺秒及epoch改變在離線窗口再次檢查。累計attempted event counts/event types也寫入health，包含event queue overflow前的計數。

每個Diagnostics instance獨立且有界：

| 檔案 | 保存規則 |
|---|---|
| `events-<clock>-000001.jsonl` 等 | 每段最多約8 MiB，保留最近8段（64 MiB）；只刪除本instance建立、同目錄的最舊closed detail段，不刪舊run或WAV。 |
| `events-<clock>-summary.jsonl` | 獨立32 MiB預算，1Hz窗口資料，不因idle detail耗盡而停止。正常固定identity數量可保存30分鐘；頻繁epoch變化或特別大量摘要仍可能達cap，不能承諾任意輸入永久記錄。 |
| `events-<clock>-faults.jsonl` | 所有admitted錯誤事件的獨立32 MiB archive，不抽樣合併。達限會明報error；health/summary計數仍能指出缺失，但不能替代已遺失的個別事件細節。 |
| `events-<clock>-manifest.json` | retained detail paths/time extents、detail_evicted_through_us、summary/archive paths及budgets；輪替時／約每秒以同目錄tmp+rename更新。檔案尾部與crash可能不完整。 |

每run主要disk budget約128 MiB（加manifest），不是整個Diagnostics目錄全球quota；不同run與capture不自動刪除。磁碟失敗或reserve cap會透過原event_log_error顯示。detail segment的RX時間範圍取min/max，因producer drain行序未必timestamp順序；淘汰範圍是保守標示，不能當逐record精確完整性證明。

API continuity及WAV sidecar新增 `drift_manifest_path`／`drift_summary_path`。`event_log_path`是當時current detail segment，不代表整個run；分析長窗或跨segment capture應傳manifest。保存重要故障時，capture完成後等3秒，將retained segments、manifest、summary與faults一起複製到另存資料夾；running process可能繼續輪替舊detail。

## 離線估測

```powershell
python -X utf8 scripts/analyze-drift.py Diagnostics/events-EXAMPLE-manifest.json --output artifacts/drift-estimates.jsonl
python -X utf8 scripts/analyze-continuity.py Diagnostics/events-EXAMPLE-manifest.json --capture Diagnostics/capture-EXAMPLE.json --output artifacts/continuity-report.jsonl
```

`analyze-drift.py`逐行讀摘要，最多16個epoch histories、每個120秒，不把30分鐘逐callback log載入RAM。對10/30/60/120秒窗口輸出：source sample/RX秒、device requested frames/RX秒、valid input消耗、source-host/device-host/source-device apparent ppm、source fit count/duration/residual RMS、個別jitter/ring均值斜率、valid/invalid reasons與母體。

source u32逐點unwrap；不接受half-range/backwards discontinuity。窗口秒缺失／重複、跨output format/epoch、資料品質fault均不輸出有效ppm。RX-only無callback時可有source_host_valid的apparent slope，但overall valid=false、device-relative ppm=null；不假造裝置速率。ppm有效只代表此觀測窗口可擬合：confidence明示network與dispatch delay不可識別，**不是crystal calibration或RF零丟包**。occupancy slope仍附fit/count，即使整體invalid也可作描述性證據；不能當控制結果。

此Stage的estimator是離線機器可讀資料；沒有將ppm回饋到播放器。原continuity分析支援manifest並顯示detail淘汰範圍，也讀取fault archive，僅去除與detail重複的archive複本，保留真正重複發生的事件數量。缺少的舊detail不能由1Hzsummary還原500ms逐packet因果，這些archive事件會因缺前史而標unknown。

## 可重現測試

```powershell
$env:TEMP = Join-Path (Get-Location) 'artifacts/continuity-diagnosis/test-temp'
$env:TMP = $env:TEMP
New-Item -ItemType Directory -Force $env:TEMP | Out-Null
cargo test --workspace --locked --target-dir target/drift-validation
python -X utf8 tests/drift-analysis-test.py
python -X utf8 tests/continuity-analysis-test.py
$env:MIC_DRIFT_ARTIFACT_DIR = Join-Path (Get-Location) 'artifacts/drift-stage-a'
cargo test -p mic-audio --locked --target-dir target/drift-validation thirty_virtual_minutes -- --ignored --nocapture
cargo test -p mic-audio --locked --target-dir target/drift-validation export_positive_50ppm -- --ignored --nocapture
```

長矩陣明確opt-in，避免一般workspace檢查每次耗時。actual Jitter/Declicker/Renderer模擬各1800秒，TX-only與device-only的0/±50/±100/±500/±1000 ppm；RX tick固定10ms，device test format16k、160-frame callback，TX source timing與device timing独立。fractional event times使用f64 ns，保留10µs級差異，不用整數ms量化；沒有引入virtual ratio compensation。tone/44.1/48k及epoch/format行為由既有renderer tests另覆蓋，不宣稱長矩陣涵蓋所有裝置格式。

每case跑diagnostic on/off，比较完整PCM rolling checksum、missing slots／capacity drops／callback starvation／ring clear與queue高水位；assert J≤8、ring≤1600、pending network模型<16。模型以actual Jitter/Renderer執行，但不是完整Tokio/UDP socket/CPAL thread模擬：feeder清ring／標epoch複用基準規則，不是Production Playback::feed closure。另測可重現single loss、80ms delayed packet及reorder；empty reanchor已補零slots的基準保留。沒有把未补偿时的缺包／溢位當測試失敗，也沒有宣稱±1000ppm已補償。

Python tests用已知header與callback rate驗證signed ppm／timestamp wrap／獨立device clock、missing秒/fault/epoch/overflow窗口invalid、1800秒streaming histories有界、delay slope可偽裝drift。Rust保存tests強制小segment輪替，驗證detail有淘汰、fault archive仍保留每個事件、summary reserve cap明報。這些是可重現software evidence，非實機聽音。

## 實機仍待驗證

本次不flash ESP32、不替換使用者運行中的RX、不執行Stage B–D。新binary需由獨立port/profile啟動；沿用既有capture API與pairing。

## 本次測得的未補償基準

`artifacts/drift-stage-a/clock-matrix.json`是18個獨立clock cases（含兩個相同0ppm對照），每case1800虛擬秒、on/off各跑一次；診斷on/off checksum和所有結果相同。這份長矩陣執行574.41秒wall time，不能混稱1800秒的Windows實機測試。

| TX ppm | device ppm | jitter missing slots | jitter capacity drops | callback starvation | ring backlog clears |
|---:|---:|---:|---:|---:|---:|
| 0 | 0 | 0 | 0 | 0 | 0 |
| -50 | 0 | 6 | 0 | 2 | 0 |
| +50 | 0 | 11676 | 11680 | 0 | 0 |
| +100 | 0 | 49607 | 49619 | 0 | 0 |
| +500 | 0 | 129806 | 129890 | 0 | 0 |
| +1000 | 0 | 129957 | 130133 | 1 | 0 |
| 0 | -1000 | 0 | 0 | 0 | 29 |
| 0 | +1000 | 0 | 0 | 180 | 0 |

故障數是actual Jitter/Renderer在特定virtual timeline、tie ordering、test訊號與基準feed策略下的結果，不是使用者ESP32/RX的實測丟包率。正TX速率差在map full且expected hole時可導致大量capacity rejection／補零，而device-only失配表現在ring；這是兩段clock邊界分開量測的依據。未補償水位有界不等於播放健康，不能拿reset後平台當drift已解決。Stage A把此行為記錄／重現，不修正。

另輸出 `positive-50ppm-occupancy.json`：逐虛擬秒 jitter/ring水位、累計missing/capacity drops，可定位容量與連續性惡化的時間；保存benchmark尚未補償，不用其曲線確診實機原因。

此trace有1799筆逐秒點，第一筆容量丟棄出現在虛擬1200秒；1799秒的jitter7包、ring318 samples、missing11649／capacity drops11652。最後一秒尾端的完整累計見矩陣1800秒結果，不能混用不同結束窗口。focused trace也驗證on/off完全相同。

## 本次驗證與修改範圍

* Workspace Rust：79個一般tests passed，兩個長測試預設ignored，另已分別實際執行並通過18-case矩陣與focused trace。
* Python：6個drift tests與15個continuity analysis tests passed（包含archive複本去重不隱藏重複事件）。
* Clippy workspace/all-targets `-D warnings`、Rust fmt、diff check及獨立Debug build通過。
* `MIC_TEST_PROFILE=drift-validation/debug tests/diagnostics-integration.py`：真localhost UDP、API/WebSocket、disabled/reception、實際30秒capture timeout、source/tick擴充、summary、manifest與headless drift unknown通過。
* 相同profile `tests/diagnostics-output-integration.py`：真Windows CPAL消耗／非空capture與fixed ratio／output epochs通過；不是30分鐘實機／loopback。
* Stage A off的既有 `tests/integration.py`：34個API/UDP/Session/output checks通過。

主要修改：`mic-audio/{diagnostics,drift,lib,output,capture}.rs`、`clock_drift_tests.rs`、`mic-network/lib.rs`；`scripts/analyze-drift.py`及continuity/verify；drift/continuity/integration tests；本文件及前次architecture review、continuity文件索引。沒有firmware/Protocol/frontend變更。生成的JSONL/WAV/矩陣/curve保持在ignored artifacts，沒有提交capture、device憑證或執行檔。

下一次實機至少30分鐘：鎖TX firmware／RX binary hash／flags/output format/jitter，保存serial、manifest+summary+faults、症狀附近detail與30秒WAV，同時Windows loopback。比較Stage A on/off下的CPU/working-set/queue drops／callback timing；分開RX logical continuity、callback/ring continuity、loopback continuity。尚無本次30分鐘ESP32或loopback驗收，不能用virtual1800秒或短CPAL測試替代。
