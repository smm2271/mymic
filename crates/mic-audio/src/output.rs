//! The same allocation-free renderer used by CPAL and offline regression tests.
use crate::Resampler;
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
    pub short: bool,
    pub consumed: u64,
    pub rendered: bool,
}
pub(crate) struct Renderer {
    sampler: Resampler,
    smoother: Smoother,
    epoch: u64,
    pending: Option<Input>,
}
impl Renderer {
    pub fn new(rate: u32) -> Self {
        Self {
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
        let mut stats = RenderStats::default();
        let mut starved_batch = false;
        for frame in data.chunks_mut(channels) {
            let current = epoch.load(Ordering::Acquire);
            if current != self.epoch {
                self.epoch = current;
                self.sampler.reset();
                self.smoother.interrupt();
            }
            let ready = !starved_batch
                && active.load(Ordering::Acquire)
                && (self.sampler.primed || queue.len() >= 320);
            let raw = if ready {
                let (value, short, faded) = self.sampler.next_tagged(|| {
                    // Bounded stale-epoch removal, even during a concurrent clear.
                    for _ in 0..queue.capacity() {
                        if epoch.load(Ordering::Acquire) != current {
                            return None;
                        }
                        let input = self.pending.take().or_else(|| queue.pop())?;
                        if input.epoch == current {
                            stats.consumed += 1;
                            return Some(input);
                        }
                        if epoch.load(Ordering::Acquire) != current {
                            self.pending = Some(input);
                            return None;
                        }
                    }
                    None
                });
                stats.rendered = true;
                if short {
                    stats.short = true;
                    starved_batch = true;
                    self.sampler.reset();
                    None
                } else {
                    Some((value, faded))
                }
            } else {
                self.sampler.reset();
                None
            };
            // This is the final mono sample, immediately before format conversion
            // and duplication to channels. No extra frame/lookahead delay is added.
            let sample = self.smoother.sample(raw);
            for out in frame {
                *out = T::from_sample(sample);
            }
        }
        stats
    }
}
