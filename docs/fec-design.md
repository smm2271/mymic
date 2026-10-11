# 4+1 XOR FEC

每個對齊的四包分組 base=`audio_sequence & !3`，parity PCM payload 逐 byte XOR；Rust 的 i16 XOR 與 byte XOR 等價，wire 仍是 S16LE。Parity type=2，sequence 是 group base、timestamp 是 base 的首樣本，不占用音訊 sequence。資料與 parity 都用 Session key HMAC。分組鍵是 Session＋transport stream token＋base；frame count與各 shard 的 source timestamp 必須一致。

RX 僅將通過 format、HMAC、Session、stream、FEC-enabled 檢查的包送入 cache。四包到齊直接使用 PCM；parity＋三份資料才可還原唯一缺失 shard，PCM 經原 Jitter admission/deadline/Declick/Playback 路徑。兩包以上遺失不宣稱成功；parity 遺失但四包到齊不需要修復。完全沒有觀察到資料／parity 的分組不在 cache，不能從「unrecoverable=0」推論無網路遺失。

Cache 最多 32 groups，每組 first-arrival 起 250 ms；完成組亦短期保留做去重。滿載淘汰最早入列組並計 capacity_evictions，未完成組計 unrecoverable。notices 最多128，notice_drops 明確可觀測；生產 receiver 每次操作立即將每條 notice 送往既有 Diagnostics bounded queue，不合併多組 TTL 錯誤。writer queue overflow 仍有既有 drop counters。

FEC 不阻塞 playout、不增加等待期限。每次實際 playout後保存 monotonic modular deadline watermark；empty rebuffer 的 expected=None 不能讓修復回到已消耗槽。只在明確新 START stream 才清 watermark。已落後 current cursor／watermark 的修復不入列，記 FEC_TOO_LATE。Wrap 用 signed modular distance，window遠小於2³¹；超長停滯跨半圈不在此保證範圍。

| Counter/event | 母體與一次增加的意義 |
|---|---|
| FEC_PARITY_RECEIVED / parity_received | 已驗證 parity 到達次數，包含重複；不是音訊包數 |
| FEC_RECOVERED / recovered | 唯一缺口 PCM 算術還原且未落後 deadline；不保證 Jitter admission／最終聲音輸出 |
| FEC_TOO_LATE / too_late | 算術可還原，但修復序號已消耗／補償；不重新入列 |
| FEC_UNRECOVERABLE / unrecoverable | 曾觀察到的未完成分組於 TTL／capacity retirement 結束；包含 parity 不存在，不能直接當成遺失包數 |
| duplicates | cache 內同一 data/parity slot 重複到達；完成組不再次 repair |
| capacity_evictions | 分組上限造成 eviction 次數，與 unrecoverable 可同時增加 |
| notice_drops | FEC notice queue 超限，診斷不完整；PCM 處理仍繼續 |

`Metrics.received_packets` 是既有 Jitter push counter，包含修復包；`received_datagrams` 是 socket datagram 次數，包含 parity／invalid。新增 `authenticated_audio_packets`／`authenticated_pcm_bytes` 只計真正已驗證的audio到達及80/160樣本的實際payload bytes；Device Telemetry 的RX packet rate／PCM bitrate採這兩個raw population，不把FEC修復當成網路接收，也沒有320-byte硬編碼。UDP sequence tracking也只觀察真正 authenticated data arrival，修復包不掩蓋 raw hole。全域 FEC counters 不因 START 清零，Lab 場次使用 window差值，stream切換標示 invalid。每條新事件含 RX monotonic timestamp、diagnostic stream/Session/clock context、playout epoch、packet/group seq、expected seq、水位、target與reason。

TX transport accumulator先納入 PCM再 send，不因單一 UDP send失敗排除 shard。若 generation queue 遺失、排程跳過產生或完整四 shard未經 transport，該組不送偽造 parity。沒有 I²S／裝置時鐘同步的假設。5 ms parity 最早在 fourth shard 的生成時刻可用（相距15 ms）；10 ms相距30 ms。目標過小或網路延遲可使修復逾期；FEC不修复兩包以上遺失，也不修復 clock drift。

事件的`deadline_sequence`是FEC保留的第一個未消耗槽，`expected_sequence`仍是當前Jitter cursor；empty rebuffer後兩者可能一個有值、一個為null，不能混淆。最末次空deadline即使清掉Jitter anchor，也必須先將FEC watermark推進；有專項測試保護這個邊界。
