#pragma once
#include <stdint.h>
typedef enum { AUDIO_SINE, AUDIO_SWEEP, AUDIO_PULSE, AUDIO_SILENCE } audio_kind_t;
typedef struct { audio_kind_t kind; uint64_t sample; double phase; } audio_source_t;
/* Future I2S implementations can implement this interface without transport changes. */
void audio_source_frame(audio_source_t *source, float gain, int mute, int16_t out[160]);
