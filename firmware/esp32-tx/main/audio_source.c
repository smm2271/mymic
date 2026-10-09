#include "audio_source.h"
#include <math.h>
void audio_source_frame(audio_source_t *s, float gain, int mute, int16_t out[160]) {
 if(gain<0)gain=0;if(gain>1)gain=1;
 for(int i=0;i<160;i++){double t=(double)s->sample/16000.0;double frequency=s->kind==AUDIO_SWEEP?200.0+1800.0*fmod(t,5.0)/5.0:440.0;s->phase=fmod(s->phase+frequency/16000.0,1.0);double value=0;
 switch(s->kind){case AUDIO_SINE:case AUDIO_SWEEP:value=sin(s->phase*6.283185307179586);break;case AUDIO_PULSE:value=fmod(t,1.0)<0.02?0.5:0;break;case AUDIO_SILENCE:break;}out[i]=mute?0:(int16_t)(value*32767*0.15*gain);s->sample++;}
}
