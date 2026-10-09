# 目前限制與下一階段

## 尚未驗收

* 本機未找到 ESP-IDF／idf.py／IDF_PATH，**ESP-IDF完整建置未執行**；C host golden vectors不代表韌體已編譯或燒錄成功。
* 未確認連接 ESP32-WROOM-32，沒有 Wi-Fi 實機 streaming 或斷線恢復測試。COM3 USB serial不是足夠硬體辨識依據。
* Windows 已列出4個output並成功開啟／切換 CPAL WASAPI stream；沒有聲學聽測、loopback錄音或USB／藍牙專項測試，不宣稱已聽到聲音。
* 沒有實測50 ms／30 ms端到端延遲。16 kHz、10 ms封包和30 ms初始jitter是配置，不是延遲量測；OS/device buffers会增加延遲。
* 未執行四小時 soak。提供 `tests/soak.py`，應在已核准的實機串流上跑滿四小時並人工記錄播放連續性。

## PoC 取捨

* 可探索64個device、多個可配對；**同時只播放一個TX**，不混音。
* Pairing trust只存在記憶體，服務／裝置重開需核准。128-bit random code只在裝置終端顯示；authenticated UDP未加密。無持久 trust store／撤銷／DTLS／完整replay protection／防DoS。
* Discovery本身未認證，可能遭垃圾裝置填滿registry；commitment驗證與HMAC防止僅用Device ID/IP冒充已核准裝置，不防廣播DoS。
* 最近32個control request快取、最近64個audio序號追蹤，只提供有限去重／重試處理。Firmware對超出快取的舊控制序號有基本拒絕；Simulator需避免作為安全測試基準。
* 線性重取樣適合PoC測試音，沒有高品質帶限filter／自適應clock drift control。漂移／排程停頓用有界queue和rebuffer控制積壓，會有短暫靜音。
* 使用Windows output default configuration；只支援F32/I16/U16，不覆蓋所有sample formats。輸出ID為當次enum索引，hotplug後需要重新列舉；輸出拔除時顯示error，沒有自動選擇替代裝置。
* Gain/mute在TX作用，封包控制有ACK和retry，不是瞬時本機淡入淡出。已設定15% full scale振幅上限；聲學輸出音量取決於OS和硬體。
* Batch controls逐項確認而非atomic transaction，部分成功可能保留；API以實際ACK結果更新狀態。
* receive bitrate是累積平均；loss是playout deadline缺包計數，亂序／late packet和scheduler stall會影響它。它們不是無線RF測量或端到端latency。
* HTTP僅IPv4 loopback，UI以桌面為主，手機RWD排版不代表開放LAN HTTP。要手機連線需另設安全代理與授權，不直接開放0.0.0.0。
* HTTP parser／WebSocket upgrade等框架錯誤可能不是domain JSON格式；一般API JSON/domain validation已統一。

## 優先順序

1. 安裝ESP-IDF，以esp32 target完整build並處理SDK/compiler診斷；確認實機再燒錄。
2. 同LAN實機測配對、四種音源、Wi-Fi drop、輸出hotplug、UI關閉後播放。
3. Windows loopback／聲學錄音確認440Hz與SRC，再量測generator首樣本→speaker聲学輸出的延遲。
4. 四小時soak並檢查memory、queue depths、音訊中断與recoverability。
5. 使用bandlimited/adaptive resampler、WASAPI buffer調整、完整anti-replay與更穩定output IDs。

本次範圍不包含實體麥克風、虛擬麥克風、VB-CABLE、Discord／OBS輸入整合。
