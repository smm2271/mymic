//! The same allocation-free renderer used by CPAL and offline regression tests.
use crate::{IsolationOptions, Resampler};
use cpal::FromSample;
use crossbeam_queue::ArrayQueue;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone, Copy)]
pub(crate) struct Input {
    pub value: f32,
    pub upstream_faded: bool,
    pub epoch: u64,
}
enum Transition {
    Normal,
    Silence {
        anchor: f32,
        position: u32,
    },
    Recovery {
        anchor: f32,
        position: u32,
        upstream_faded: bool,
    },
}
struct Smoother {
    last: f32,
    length: u32,
    state: Transition,
}
impl Smoother {
    fn new(rate: u32) -> Self {
        Self {
            last: 0.0,
            length: (rate * 3 / 1000).max(2),
            state: Transition::Normal,
        }
    }
    fn interrupt(&mut self) {
        if !matches!(self.state, Transition::Silence { .. }) {
            self.state = Transition::Silence {
                anchor: self.last,
                position: 0,
            };
        }
    }
    fn sample(&mut self, raw: Option<(f32, bool)>) -> f32 {
        if raw.is_none() {
            self.interrupt();
        }
        if let Some((sample, faded)) = raw
            && matches!(self.state, Transition::Silence { .. })
        {
            // Upstream already has a gain fade: only correct the boundary offset,
            // rather than multiply its fade envelope a second time.
            self.state = Transition::Recovery {
                anchor: if faded { self.last - sample } else { self.last },
                position: 0,
                upstream_faded: faded,
            };
        }
        let output = match &mut self.state {
            Transition::Normal => raw.map_or(0.0, |r| r.0),
            Transition::Silence { anchor, position } => {
                let weight = smoothstep((*position).min(self.length - 1), self.length);
                *position = position.saturating_add(1);
                *anchor * (1.0 - weight)
            }
            Transition::Recovery {
                anchor,
                position,
                upstream_faded,
            } => {
                let sample = raw.map_or(0.0, |r| r.0);
                let weight = smoothstep(*position, self.length);
                let out = if *upstream_faded {
                    sample + *anchor * (1.0 - weight)
                } else {
                    sample * weight + *anchor * (1.0 - weight)
                };
                *position += 1;
                if *position >= self.length {
                    self.state = Transition::Normal;
                }
                out
            }
        }
        .clamp(-1.0, 1.0);
        self.last = output;
        output
    }
}
fn smoothstep(position: u32, length: u32) -> f32 {
    let t = position as f32 / (length - 1) as f32;
    t * t * (3.0 - 2.0 * t)
}
#[derive(Default)]
pub(crate) struct RenderStats {
    pub zero_frames: u64,
    pub supplied_frames: u64,
    pub inserted_frames: u64,
    pub prefill_frames: u64,
    pub inactive_frames: u64,
    pub starvation_frames: u64,
    pub longest_zero_run: u64,
    pub trailing_zero_run: u64,
    pub missing_input_samples: u64,
    pub short: bool,
    pub consumed: u64,
    pub rendered: bool,
}
pub(crate) struct Renderer {
    options: IsolationOptions,
    zero_run: u64,
    sampler: Resampler,
    smoother: Smoother,
    epoch: u64,
    pending: Option<Input>,
}
impl Renderer {
    #[cfg(test)]
    pub fn new(rate: u32) -> Self {
        Self::with_options(rate, IsolationOptions::default())
    }
    pub fn with_options(rate: u32, options: IsolationOptions) -> Self {
        Self {
            options,
            zero_run: 0,
            sampler: Resampler::new(rate),
            smoother: Smoother::new(rate),
            epoch: 0,
            pending: None,
        }
    }
    pub fn primed(&self) -> bool {
        self.sampler.primed
    }
    pub fn render<T: cpal::Sample + FromSample<f32>>(
        &mut self,
        data: &mut [T],
        channels: usize,
        queue: &ArrayQueue<Input>,
        active: &AtomicBool,
        epoch: &AtomicU64,
    ) -> RenderStats {
        self.render_inner(data, channels, queue, active, epoch, |_, _| {})
    }
    #[cfg(test)]
    pub fn render_injected<T: cpal::Sample + FromSample<f32>>(
        &mut self,
        data: &mut [T],
        queue: &ArrayQueue<Input>,
        active: &AtomicBool,
        epoch: &AtomicU64,
        arrival: impl FnMut(usize, &ArrayQueue<Input>),
    ) -> RenderStats {
        self.render_inner(data, 1, queue, active, epoch, arrival)
    }
    fn render_inner<T: cpal::Sample + FromSample<f32>>(
        &mut self,
        data: &mut [T],
        channels: usize,
        queue: &ArrayQueue<Input>,
        active: &AtomicBool,
        epoch: &AtomicU64,
        mut arrival: impl FnMut(usize, &ArrayQueue<Input>),
    ) -> RenderStats {
        let mut stats = RenderStats::default();
        let mut starved_batch = false;
        let legacy_ready =
            active.load(Ordering::Acquire) && (self.sampler.primed || queue.len() >= 320);
        for (index, frame) in data.chunks_mut(channels).enumerate() {
            arrival(index, queue);
            let current = epoch.load(Ordering::Acquire);
            if self.options.recovery_policy && current != self.epoch {
                self.epoch = current;
                self.sampler.reset();
                self.smoother.interrupt();
            }
            let ready = if !self.options.recovery_policy {
                legacy_ready
            } else {
                !starved_batch
                    && active.load(Ordering::Acquire)
                    && (self.sampler.primed || queue.len() >= 320)
            };
            let raw = if ready {
                let (value, short, faded) = self.sampler.next_tagged(|| {
                    // Bounded stale-epoch removal, even during a concurrent clear.
                    for _ in 0..queue.capacity() {
                        if self.options.recovery_policy && epoch.load(Ordering::Acquire) != current
                        {
                            return None;
                        }
                        let Some(input) = self.pending.take().or_else(|| queue.pop()) else {
                            stats.missing_input_samples += 1;
                            return None;
                        };
                        if !self.options.recovery_policy || input.epoch == current {
                            stats.consumed += 1;
                            return Some(input);
                        }
                        if self.options.recovery_policy && epoch.load(Ordering::Acquire) != current
                        {
                            self.pending = Some(input);
                            return None;
                        }
                    }
                    None
                });
                stats.rendered = true;
                if short {
                    stats.short = true;
                    if self.options.recovery_policy {
                        starved_batch = true;
                        self.sampler.reset();
                        None
                    } else {
                        Some((value, faded))
                    }
                } else {
                    stats.supplied_frames += 1;
                    Some((value, faded))
                }
            } else {
                self.sampler.reset();
                None
            };
            // This is the final mono sample, immediately before format conversion
            // and duplication to channels. No extra frame/lookahead delay is added.
            if raw.is_none() {
                stats.inserted_frames += 1;
                if !active.load(Ordering::Acquire) {
                    stats.inactive_frames += 1;
                } else if starved_batch {
                    stats.starvation_frames += 1;
                } else {
                    stats.prefill_frames += 1;
                }
            }
            let sample = if self.options.output_fade {
                self.smoother.sample(raw)
            } else {
                raw.map_or(0.0, |r| r.0)
            };
            let converted = T::from_sample(sample);
            if converted == T::EQUILIBRIUM {
                stats.zero_frames += 1;
                self.zero_run += 1;
                stats.longest_zero_run = stats.longest_zero_run.max(self.zero_run);
            } else {
                self.zero_run = 0;
            }
            for out in frame {
                *out = converted;
            }
        }
        if !self.options.recovery_policy && stats.short {
            self.sampler.reset();
        }
        stats.trailing_zero_run = self.zero_run;
        stats
    }
}

#[derive(Default, Clone, Debug, serde::Serialize)]
pub struct OutputSnapshot {
    pub callback_last_monotonic_us: u64,
    pub resampler_missing_input_samples: u64,
    pub callback_intervals: u64,
    pub callback_last_interval_us: u64,
    pub callback_max_interval_us: u64,
    pub callback_mean_interval_us: f64,
    pub exact_zero_output_us: u64,
    pub longest_exact_zero_run_us: u64,
    pub current_exact_zero_run_us: u64,
    pub pipeline_inserted_us: u64,
    pub prefill_us: u64,
    pub inactive_us: u64,
    pub starvation_us: u64,
}
#[derive(Default)]
pub(crate) struct Counters {
    missing: AtomicU64,
    last: AtomicU64,
    count: AtomicU64,
    sum: AtomicU64,
    max: AtomicU64,
    interval: AtomicU64,
    rate: AtomicU64,
    zeros: AtomicU64,
    longest: AtomicU64,
    trailing: AtomicU64,
    inserted: AtomicU64,
    prefill: AtomicU64,
    inactive: AtomicU64,
    starvation: AtomicU64,
}
impl Counters {
    pub fn callback(&self, now_us: u64, rate: u32) {
        self.rate.store(rate as u64, Ordering::Relaxed);
        let time = now_us.saturating_add(1);
        let previous = self.last.swap(time, Ordering::Relaxed);
        if previous != 0 {
            let gap = time.saturating_sub(previous);
            self.count.fetch_add(1, Ordering::Relaxed);
            self.sum.fetch_add(gap, Ordering::Relaxed);
            self.max.fetch_max(gap, Ordering::Relaxed);
            self.interval.store(gap, Ordering::Relaxed);
        }
    }
    pub fn record(&self, s: &RenderStats) {
        let micros = |frames: u64| {
            frames.saturating_mul(1_000_000) / self.rate.load(Ordering::Relaxed).max(1)
        };
        self.missing
            .fetch_add(s.missing_input_samples, Ordering::Relaxed);
        for (counter, value) in [
            (&self.zeros, s.zero_frames),
            (&self.inserted, s.inserted_frames),
            (&self.prefill, s.prefill_frames),
            (&self.inactive, s.inactive_frames),
            (&self.starvation, s.starvation_frames),
        ] {
            counter.fetch_add(micros(value), Ordering::Relaxed);
        }
        self.longest
            .fetch_max(micros(s.longest_zero_run), Ordering::Relaxed);
        self.trailing
            .store(micros(s.trailing_zero_run), Ordering::Relaxed);
    }
    pub fn snapshot(&self) -> OutputSnapshot {
        let get = |a: &AtomicU64| a.load(Ordering::Relaxed);

        OutputSnapshot {
            callback_last_monotonic_us: get(&self.last).saturating_sub(1),
            resampler_missing_input_samples: get(&self.missing),
            callback_intervals: get(&self.count),
            callback_last_interval_us: get(&self.interval),
            callback_max_interval_us: get(&self.max),
            callback_mean_interval_us: get(&self.sum) as f64 / get(&self.count).max(1) as f64,
            exact_zero_output_us: get(&self.zeros),
            longest_exact_zero_run_us: get(&self.longest),
            current_exact_zero_run_us: get(&self.trailing),
            pipeline_inserted_us: get(&self.inserted),
            prefill_us: get(&self.prefill),
            inactive_us: get(&self.inactive),
            starvation_us: get(&self.starvation),
        }
    }
}
