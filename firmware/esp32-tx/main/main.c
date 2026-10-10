#include <stdio.h>
#include <stdatomic.h>
#include <errno.h>
#include <math.h>
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"
#include "freertos/queue.h"
#include "freertos/semphr.h"
#include "esp_wifi.h"
#include "esp_event.h"
#include "esp_netif.h"
#include "esp_log.h"
#include "esp_random.h"
#include "esp_timer.h"
#include "nvs_flash.h"
#include "lwip/sockets.h"
#include "protocol.h"
#include "audio_source.h"
static const char *TAG = "mic-tx";
typedef struct
{
    int wifi, paired, streaming, mute;
    float gain;
    audio_kind_t source;
    char nonce[33], session[33];
    uint8_t key[32], session_bytes[16];
    struct sockaddr_in peer, audio;
    int64_t last;
    uint32_t epoch;
} context_t;
typedef struct
{
    uint32_t sequence, timestamp, epoch;
    uint8_t session[16], key[32];
    struct sockaddr_in destination;
    int16_t samples[160];
    int64_t generated_us, enqueued_us;
} frame_t;
typedef struct
{
    uint32_t packets, interval_count;
    uint32_t send_errors;
    uint32_t late_frames;
    uint32_t queue_drops;
    int64_t min_interval_us;
    int64_t max_interval_us;
    int64_t sum_interval_us;
    double sum_sq_interval_us;
    int64_t max_queue_delay_us;
    int64_t max_send_duration_us;
    uint32_t window_id, last_sequence, queue_depth_max;
    int64_t start_us, end_us, generation_max_us, preparation_max_us;
    int64_t generated_us, enqueued_us, dequeued_us, send_started_us, completed_us;
} tx_metrics_window_t;
typedef struct
{
    uint32_t epoch;
    uint32_t window_packets, window_successful, window_id, window_queue_depth_max;
    int64_t window_generation_max_us, window_preparation_max_us;
    uint32_t window_send_errors;
    uint32_t window_late_frames;
    int64_t window_min_interval_us;
    int64_t window_max_interval_us;
    int64_t window_sum_interval_us;
    double window_sum_sq_interval_us;
    int64_t window_max_queue_delay_us;
    int64_t window_max_send_duration_us;
    int64_t last_sendto_completed_us;
    int64_t window_start_us;
    tx_metrics_window_t published;
    int has_published;
} tx_timing_state_t;
static tx_timing_state_t tx_timing;
static portMUX_TYPE timing_lock = portMUX_INITIALIZER_UNLOCKED;
static context_t ctx;
static SemaphoreHandle_t mutex;
static QueueHandle_t frames;
static char device_id[32], code[33];
static int control_fd, discovery_fd, audio_fd;
static _Atomic uint32_t requests, drops, packets, generated_frames, queue_drops;
static void random_hex(char out[33])
{
    uint8_t b[16];
    esp_fill_random(b, sizeof b);
    mic_hex(b, 16, out);
}
static context_t snapshot(void)
{
    context_t c;
    xSemaphoreTake(mutex, portMAX_DELAY);
    c = ctx;
    xSemaphoreGive(mutex);
    return c;
}
static void send_json(cJSON *m, const struct sockaddr_in *peer)
{
    char *b = cJSON_PrintUnformatted(m);
    if (b)
    {
        sendto(control_fd, b, strlen(b), 0, (const struct sockaddr *)peer, sizeof *peer);
        free(b);
    }
    cJSON_Delete(m);
}
static int same_peer(struct sockaddr_in a, struct sockaddr_in b) { return a.sin_addr.s_addr == b.sin_addr.s_addr && a.sin_port == b.sin_port; }
static void reset_connection(void)
{
    xSemaphoreTake(mutex, portMAX_DELAY);
    ctx.paired = ctx.streaming = 0;
    ctx.epoch++;
    ctx.session[0] = 0;
    random_hex(ctx.nonce);
    xSemaphoreGive(mutex);
    portENTER_CRITICAL(&timing_lock);
    uint32_t previous_window_id = tx_timing.window_id;
    memset(&tx_timing, 0, sizeof(tx_timing));
    tx_timing.window_id = previous_window_id;
    portEXIT_CRITICAL(&timing_lock);
    xQueueReset(frames);
}
static void wifi_event(void *arg, esp_event_base_t base, int32_t id, void *data)
{
    if (base == WIFI_EVENT && id == WIFI_EVENT_STA_START)
        esp_wifi_connect();
    if (base == WIFI_EVENT && id == WIFI_EVENT_STA_DISCONNECTED)
    {
        xSemaphoreTake(mutex, portMAX_DELAY);
        ctx.wifi = 0;
        xSemaphoreGive(mutex);
        reset_connection();
        esp_wifi_connect();
    }
    if (base == IP_EVENT && id == IP_EVENT_STA_GOT_IP)
    {
        reset_connection();
        xSemaphoreTake(mutex, portMAX_DELAY);
        ctx.wifi = 1;
        xSemaphoreGive(mutex);
        ESP_LOGI(TAG, "Wi-Fi ready; awaiting explicit pairing approval");
    }
}
static void discovery_task(void *arg)
{
    struct sockaddr_in broadcast = {.sin_family = AF_INET, .sin_port = htons(CONFIG_MIC_DISCOVERY_PORT), .sin_addr.s_addr = htonl(INADDR_BROADCAST)};
    while (1)
    {
        context_t c = snapshot();
        if (c.wifi && !c.paired)
        {
            cJSON *m = mic_message("DISCOVER", device_id, ++requests);
            char commitment[65];
            mic_commitment(code, c.nonce, commitment);
            cJSON_AddStringToObject(m, "name", CONFIG_MIC_DEVICE_NAME);
            cJSON_AddStringToObject(m, "device_type", "esp32-wroom-32");
            cJSON *a = cJSON_AddArrayToObject(m, "capabilities");
            const char *sources[] = {"sine", "sweep", "pulse", "silence"};
            for (int i = 0; i < 4; i++)
                cJSON_AddItemToArray(a, cJSON_CreateString(sources[i]));
            cJSON_AddStringToObject(m, "nonce", c.nonce);
            cJSON_AddStringToObject(m, "commitment", commitment);
            cJSON_AddNumberToObject(m, "control_port", CONFIG_MIC_CONTROL_PORT);
            char *b = cJSON_PrintUnformatted(m);
            if (b)
            {
                sendto(discovery_fd, b, strlen(b), 0, (struct sockaddr *)&broadcast, sizeof broadcast);
                free(b);
            }
            cJSON_Delete(m);
        }
        vTaskDelay(pdMS_TO_TICKS(1000));
    }
}
static void control_task(void *arg)
{
    char buf[2049];
    uint32_t cached_id[32] = {0};
    char *cached_reply[32] = {0};
    int cached_count = 0;
    uint32_t last_request = 0;
    int have_request = 0;
    char cached_session[33] = "";
    while (1)
    {
        struct sockaddr_in from;
        socklen_t size = sizeof from;
        int n = recvfrom(control_fd, buf, sizeof(buf) - 1, 0, (struct sockaddr *)&from, &size);
        if (n <= 0)
            continue;
        buf[n] = 0;
        cJSON *m = cJSON_ParseWithLength(buf, n);
        if (!m)
            continue;
        cJSON *rid = cJSON_GetObjectItemCaseSensitive(m, "request_id");
        const char *kind = mic_str(m, "kind");
        if (mic_num(m, "version") != 1 || !cJSON_IsNumber(rid) || rid->valuedouble < 0 || rid->valuedouble > 4294967295.0 || strcmp(mic_str(m, "device_id"), device_id) || strlen(kind) > 32 || strlen(mic_str(m, "nonce")) > 32 || strlen(mic_str(m, "session")) > 32)
        {
            cJSON_Delete(m);
            continue;
        }
        uint32_t request = (uint32_t)rid->valuedouble;
        context_t c = snapshot();
        if (strcmp(c.session, cached_session))
        {
            for (int i = 0; i < 32; i++)
            {
                free(cached_reply[i]);
                cached_reply[i] = NULL;
            }
            cached_count = 0;
            have_request = 0;
            strcpy(cached_session, c.session);
        }
        if (!strcmp(kind, "OFFER"))
        {
            cJSON_Delete(m);
            continue;
        }
        if (!strcmp(kind, "PAIR_REQUEST"))
        {
            uint8_t session_bytes[16];
            int port = mic_num(m, "audio_port");
            if (strcmp(mic_str(m, "nonce"), c.nonce) || mic_unhex(mic_str(m, "session"), 16, session_bytes) || port <= 0 || port > 65535 || !mic_verify(m, (uint8_t *)code, 32) || (c.paired && (!same_peer(c.peer, from) || strcmp(c.session, mic_str(m, "session")))))
            {
                cJSON_Delete(m);
                continue;
            }
            char material[80];
            snprintf(material, sizeof material, "%s|%s", c.nonce, mic_str(m, "session"));
            uint8_t key[32];
            mic_hmac((uint8_t *)code, 32, (uint8_t *)material, strlen(material), key);
            xSemaphoreTake(mutex, portMAX_DELAY);
            ctx.peer = from;
            ctx.audio = from;
            ctx.audio.sin_port = htons(port);
            ctx.paired = 1;
            ctx.last = esp_timer_get_time();
            strcpy(ctx.session, mic_str(m, "session"));
            memcpy(ctx.session_bytes, session_bytes, 16);
            memcpy(ctx.key, key, 32);
            xSemaphoreGive(mutex);
            cJSON *reply = mic_message("PAIR_ACCEPT", device_id, request);
            cJSON_AddStringToObject(reply, "session", mic_str(m, "session"));
            mic_sign(reply, key, 32);
            send_json(reply, &from);
            cJSON_Delete(m);
            continue;
        }
        if (!c.paired || !same_peer(c.peer, from) || strcmp(mic_str(m, "session"), c.session) || !mic_verify(m, c.key, 32))
        {
            cJSON_Delete(m);
            continue;
        }
        int found = 0;
        for (int i = 0; i < 32; i++)
        {
            if (cached_reply[i] && cached_id[i] == request)
            {
                sendto(control_fd, cached_reply[i], strlen(cached_reply[i]), 0, (struct sockaddr *)&from, sizeof from);
                found = 1;
                break;
            }
        }
        if (found)
        {
            cJSON_Delete(m);
            continue;
        }
        if (have_request && (int32_t)(request - last_request) <= 0)
        {
            cJSON_Delete(m);
            continue;
        }
        last_request = request;
        have_request = 1;
        xSemaphoreTake(mutex, portMAX_DELAY);
        ctx.last = esp_timer_get_time();
        xSemaphoreGive(mutex);
        if (!strcmp(kind, "HEARTBEAT"))
        {
            cJSON_Delete(m);
            continue;
        }
        const char *error = NULL;
        xSemaphoreTake(mutex, portMAX_DELAY);
        if (!strcmp(kind, "START_STREAM"))
        {
            ctx.streaming = 1;
            ctx.epoch++;
            xQueueReset(frames);
        }
        else if (!strcmp(kind, "STOP_STREAM"))
        {
            ctx.streaming = 0;
            ctx.epoch++;
            xQueueReset(frames);
        }
        else if (!strcmp(kind, "SET_AUDIO_SOURCE"))
        {
            const char *s = mic_str(m, "source");
            if (!strcmp(s, "sine"))
                ctx.source = AUDIO_SINE;
            else if (!strcmp(s, "sweep"))
                ctx.source = AUDIO_SWEEP;
            else if (!strcmp(s, "pulse"))
                ctx.source = AUDIO_PULSE;
            else if (!strcmp(s, "silence"))
                ctx.source = AUDIO_SILENCE;
            else
                error = "invalid source";
        }
        else if (!strcmp(kind, "SET_GAIN"))
        {
            cJSON *g = cJSON_GetObjectItemCaseSensitive(m, "gain");
            if (!cJSON_IsNumber(g) || !isfinite(g->valuedouble) || g->valuedouble < 0 || g->valuedouble > 1)
                error = "invalid gain";
            else
                ctx.gain = (float)g->valuedouble;
        }
        else if (!strcmp(kind, "SET_MUTE"))
        {
            cJSON *v = cJSON_GetObjectItemCaseSensitive(m, "mute");
            if (!cJSON_IsBool(v))
                error = "invalid mute";
            else
                ctx.mute = cJSON_IsTrue(v);
        }
        else
            error = "unsupported command";
        xSemaphoreGive(mutex);
        cJSON *reply = mic_message(error ? "ERROR" : "ACK", device_id, request);
        cJSON_AddStringToObject(reply, "session", c.session);
        if (error)
            cJSON_AddStringToObject(reply, "error", error);
        mic_sign(reply, c.key, 32);
        int index = cached_count++ % 32;
        free(cached_reply[index]);
        cached_reply[index] = cJSON_PrintUnformatted(reply);
        cached_id[index] = request;
        send_json(reply, &from);
        cJSON_Delete(m);
    }
}
static void generation_task(void *arg)
{
    TickType_t clock = xTaskGetTickCount();
    audio_source_t source = {0};
    uint32_t sequence = 0, epoch = 0;
    while (1)
    {
        vTaskDelayUntil(&clock, pdMS_TO_TICKS(10));
        context_t c = snapshot();
        if (!c.wifi || !c.paired || !c.streaming)
            continue;
        if (epoch != c.epoch)
        {
            epoch = c.epoch;
            sequence = 0;
            source.sample = 0;
            source.phase = 0;
        }
        source.kind = c.source;
        frame_t frame = {
            .sequence = sequence++,
            .timestamp = (uint32_t)source.sample,
            .epoch = c.epoch,
            .destination = c.audio,
            .generated_us = esp_timer_get_time()
        };
        memcpy(frame.key, c.key, 32);
        memcpy(frame.session, c.session_bytes, 16);
        audio_source_frame(&source, c.gain, c.mute, frame.samples);
        generated_frames++;
        frame.enqueued_us = esp_timer_get_time();
        if (xQueueSend(frames, &frame, 0) != pdTRUE)
        {
            frame_t dropped;
            xQueueReceive(frames, &dropped, 0);
            xQueueSend(frames, &frame, 0);
            drops++;
            queue_drops++;
        }
    }
}
static void transport_task(void *arg)
{
    frame_t frame;
    uint8_t packet[MIC_PACKET_SIZE], digest[32];
    while (1)
    {
        if (xQueueReceive(frames, &frame, portMAX_DELAY) != pdTRUE)
            continue;
        int64_t dequeued_us = esp_timer_get_time();
        uint32_t queue_depth = (uint32_t)uxQueueMessagesWaiting(frames) + 1;
        context_t c = snapshot();
        if (!c.streaming || !c.paired || frame.epoch != c.epoch)
            continue;
        int64_t entry_time_us = esp_timer_get_time();
        int64_t queue_delay_us = dequeued_us - frame.enqueued_us;
        mic_encode_body(packet, frame.sequence, frame.timestamp, frame.session, frame.samples);
        mic_hmac(frame.key, 32, packet, 352, digest);
        memcpy(packet + 352, digest, 16);
        int64_t send_started_us = esp_timer_get_time();
        int res = sendto(audio_fd, packet, sizeof packet, 0, (struct sockaddr *)&frame.destination, sizeof frame.destination);
        int64_t completed_time_us = esp_timer_get_time();
        int64_t send_duration_us = completed_time_us - send_started_us;
        portENTER_CRITICAL(&timing_lock);
        if (tx_timing.epoch != frame.epoch) {
            uint32_t previous_window_id = tx_timing.window_id;
            memset(&tx_timing, 0, sizeof(tx_timing));
            tx_timing.window_id = previous_window_id;
            tx_timing.epoch = frame.epoch;
        }
        if (tx_timing.window_start_us == 0) {
            tx_timing.window_start_us = entry_time_us;
            tx_timing.window_min_interval_us = INT64_MAX;
        }
        if (queue_delay_us > tx_timing.window_max_queue_delay_us) tx_timing.window_max_queue_delay_us = queue_delay_us;
        if (send_duration_us > tx_timing.window_max_send_duration_us) tx_timing.window_max_send_duration_us = send_duration_us;
        int64_t generation_us = frame.enqueued_us - frame.generated_us;
        int64_t preparation_us = send_started_us - entry_time_us;
        if (generation_us > tx_timing.window_generation_max_us) tx_timing.window_generation_max_us = generation_us;
        if (preparation_us > tx_timing.window_preparation_max_us) tx_timing.window_preparation_max_us = preparation_us;
        if (queue_depth > tx_timing.window_queue_depth_max) tx_timing.window_queue_depth_max = queue_depth;
        if (res == sizeof packet)
        {
            packets++;
            tx_timing.window_successful++;
            if (tx_timing.last_sendto_completed_us > 0)
            {
                int64_t interval_us = completed_time_us - tx_timing.last_sendto_completed_us;
                if (interval_us > 0)
                {
                    tx_timing.window_packets++;
                    if (interval_us < tx_timing.window_min_interval_us)
                        tx_timing.window_min_interval_us = interval_us;
                    if (interval_us > tx_timing.window_max_interval_us)
                        tx_timing.window_max_interval_us = interval_us;
                    tx_timing.window_sum_interval_us += interval_us;
                    tx_timing.window_sum_sq_interval_us += (double)interval_us * interval_us;
                    if (interval_us > 15000)
                        tx_timing.window_late_frames++;

                }
            }
            tx_timing.last_sendto_completed_us = completed_time_us;
        }
        else
        {
            drops++;
            tx_timing.window_send_errors++;
        }
        if (completed_time_us - tx_timing.window_start_us >= 1000000)
        {
            tx_timing.published = (tx_metrics_window_t){
                .packets = tx_timing.window_successful,
                .interval_count = tx_timing.window_packets,
                .window_id = ++tx_timing.window_id,
                .start_us = tx_timing.window_start_us, .end_us = completed_time_us,
                .generation_max_us = tx_timing.window_generation_max_us, .preparation_max_us = tx_timing.window_preparation_max_us,
                .queue_depth_max = tx_timing.window_queue_depth_max, .last_sequence = frame.sequence,
                .generated_us = frame.generated_us, .enqueued_us = frame.enqueued_us, .dequeued_us = dequeued_us,
                .send_started_us = send_started_us, .completed_us = completed_time_us,
                .send_errors = tx_timing.window_send_errors,
                .late_frames = tx_timing.window_late_frames,
                .queue_drops = atomic_load(&queue_drops),
                .min_interval_us = tx_timing.window_min_interval_us,
                .max_interval_us = tx_timing.window_max_interval_us,
                .sum_interval_us = tx_timing.window_sum_interval_us,
                .sum_sq_interval_us = tx_timing.window_sum_sq_interval_us,
                .max_queue_delay_us = tx_timing.window_max_queue_delay_us,
                .max_send_duration_us = tx_timing.window_max_send_duration_us,
            };
            tx_timing.has_published = 1;
            tx_timing.window_packets = 0;
            tx_timing.window_successful = 0;
            tx_timing.window_generation_max_us = tx_timing.window_preparation_max_us = 0;
            tx_timing.window_queue_depth_max = 0;
            tx_timing.window_send_errors = 0;
            tx_timing.window_late_frames = 0;
            tx_timing.window_min_interval_us = INT64_MAX;
            tx_timing.window_max_interval_us = 0;
            tx_timing.window_sum_interval_us = 0;
            tx_timing.window_sum_sq_interval_us = 0;
            tx_timing.window_max_queue_delay_us = 0;
            tx_timing.window_max_send_duration_us = 0;
            tx_timing.window_start_us = completed_time_us;
        }
        portEXIT_CRITICAL(&timing_lock);
    }
}
static void telemetry_task(void *arg)
{
    while (1)
    {
        context_t c = snapshot();
        if (c.paired)
        {
            if (esp_timer_get_time() - c.last > 5000000)
            {
                reset_connection();
            }
            else
            {
                cJSON *m = mic_message("HEARTBEAT", device_id, ++requests);
                cJSON_AddStringToObject(m, "session", c.session);
                mic_sign(m, c.key, 32);
                send_json(m, &c.peer);
            }
        }
        portENTER_CRITICAL(&timing_lock);
        int fresh = tx_timing.has_published;
        tx_metrics_window_t snap = tx_timing.published;
        tx_timing.has_published = 0;
        portEXIT_CRITICAL(&timing_lock);
        if (fresh)
        {
            int64_t mean_us = snap.interval_count > 0 ? (snap.sum_interval_us / snap.interval_count) : 0;
            int64_t std_us = 0;
            if (snap.interval_count > 1)
            {
                double mean = (double)snap.sum_interval_us / snap.interval_count;
                double variance = ((double)snap.sum_sq_interval_us - (double)snap.sum_interval_us * mean) / (snap.interval_count - 1);
                if (variance > 0)
                    std_us = (int64_t)sqrt(variance);
            }
            ESP_LOGI(TAG, "TX Timing window: %u sent, interval mean=%lld us (min=%lld, max=%lld, std=%lld us), late=%u, err=%u, q_delay_max=%lld us, send_max=%lld us",
                     (unsigned)snap.packets,
                     (long long)mean_us,
                     (long long)(snap.min_interval_us == INT64_MAX ? 0 : snap.min_interval_us),
                     (long long)snap.max_interval_us,
                     (long long)std_us,
                     (unsigned)snap.late_frames,
                     (unsigned)snap.send_errors,
                     (long long)snap.max_queue_delay_us,
                     (long long)snap.max_send_duration_us);
            ESP_LOGI(TAG, "TX Window id=%u start_us=%lld end_us=%lld duration_us=%lld rate=%.2f intervals=%u gen_total_at_report=%u queue_drops_total=%u queue_depth_max=%u gen_max_us=%lld prep_max_us=%lld",
                     (unsigned)snap.window_id, (long long)snap.start_us, (long long)snap.end_us, (long long)(snap.end_us-snap.start_us),
                     (double)snap.packets * 1000000.0 / (snap.end_us-snap.start_us), (unsigned)snap.interval_count,
                     (unsigned)atomic_load(&generated_frames), (unsigned)snap.queue_drops, (unsigned)snap.queue_depth_max,
                     (long long)snap.generation_max_us, (long long)snap.preparation_max_us);
            ESP_LOGI(TAG, "TX Frame seq=%u generated_us=%lld enqueue_attempt_us=%lld dequeued_us=%lld send_started_us=%lld completed_us=%lld report_age_us=%lld",
                     (unsigned)snap.last_sequence, (long long)snap.generated_us, (long long)snap.enqueued_us, (long long)snap.dequeued_us,
                     (long long)snap.send_started_us, (long long)snap.completed_us, (long long)(esp_timer_get_time()-snap.end_us));
        }
        else
        {
            ESP_LOGI(TAG, "TX Timing: no fresh window, streaming=%d age_us=%lld queue_now=%u", c.streaming, (long long)(snap.end_us ? esp_timer_get_time() - snap.end_us : -1), (unsigned)uxQueueMessagesWaiting(frames));
        }
        vTaskDelay(pdMS_TO_TICKS(1000));
    }
}
void app_main(void)
{
    esp_err_t result = nvs_flash_init();
    if (result == ESP_ERR_NVS_NO_FREE_PAGES || result == ESP_ERR_NVS_NEW_VERSION_FOUND)
    {
        ESP_ERROR_CHECK(nvs_flash_erase());
        result = nvs_flash_init();
    }
    ESP_ERROR_CHECK(result);
    mutex = xSemaphoreCreateMutex();
    frames = xQueueCreate(4, sizeof(frame_t));
    configASSERT(mutex && frames);
    ctx.gain = 1.0;
    random_hex(code);
    random_hex(ctx.nonce);
    ESP_ERROR_CHECK(esp_netif_init());
    ESP_ERROR_CHECK(esp_event_loop_create_default());
    esp_netif_create_default_wifi_sta();
    wifi_init_config_t init = WIFI_INIT_CONFIG_DEFAULT();
    ESP_ERROR_CHECK(esp_wifi_init(&init));
    ESP_ERROR_CHECK(esp_wifi_set_storage(WIFI_STORAGE_RAM));
    uint8_t mac[6];
    ESP_ERROR_CHECK(esp_wifi_get_mac(WIFI_IF_STA, mac));
    snprintf(device_id, sizeof device_id, "esp32-%02x%02x%02x%02x%02x%02x", mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]);
    ESP_LOGI(TAG, "Device %s pairing credential: %s", device_id, code);
    control_fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_IP);
    discovery_fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_IP);
    audio_fd = socket(AF_INET, SOCK_DGRAM, IPPROTO_IP);
    configASSERT(control_fd >= 0 && discovery_fd >= 0 && audio_fd >= 0);
    int yes = 1;
    setsockopt(discovery_fd, SOL_SOCKET, SO_BROADCAST, &yes, sizeof yes);
    struct sockaddr_in bind_addr = {.sin_family = AF_INET, .sin_port = htons(CONFIG_MIC_CONTROL_PORT), .sin_addr.s_addr = htonl(INADDR_ANY)};
    configASSERT(bind(control_fd, (struct sockaddr *)&bind_addr, sizeof bind_addr) == 0);
    ESP_ERROR_CHECK(esp_event_handler_register(WIFI_EVENT, ESP_EVENT_ANY_ID, &wifi_event, NULL));
    ESP_ERROR_CHECK(esp_event_handler_register(IP_EVENT, IP_EVENT_STA_GOT_IP, &wifi_event, NULL));
    wifi_config_t wifi = {0};
    strlcpy((char *)wifi.sta.ssid, CONFIG_MIC_WIFI_SSID, sizeof wifi.sta.ssid);
    strlcpy((char *)wifi.sta.password, CONFIG_MIC_WIFI_PASSWORD, sizeof wifi.sta.password);
    ESP_ERROR_CHECK(esp_wifi_set_mode(WIFI_MODE_STA));
    ESP_ERROR_CHECK(esp_wifi_set_config(WIFI_IF_STA, &wifi));
    ESP_ERROR_CHECK(esp_wifi_start());
    ESP_ERROR_CHECK(esp_wifi_set_ps(WIFI_PS_NONE));
    configASSERT(xTaskCreate(discovery_task, "discovery", 4096, NULL, 3, NULL) == pdPASS);
    configASSERT(xTaskCreate(control_task, "control", 8192, NULL, 5, NULL) == pdPASS);
    configASSERT(xTaskCreate(generation_task, "generation", 4096, NULL, 6, NULL) == pdPASS);
    configASSERT(xTaskCreate(transport_task, "transport", 4096, NULL, 5, NULL) == pdPASS);
    configASSERT(xTaskCreate(telemetry_task, "telemetry", 4096, NULL, 3, NULL) == pdPASS);
}
