//! The producer side of the existing mixer ring; called from Update, never its callback.

use super::{Baked, Continuous};
use crate::audio::{AudioMixer, Bus, SOURCE_CAPACITY, SourceHandle, spatial::Placement};

/// The cost decision is visible at the call site. Baked samples are shared, continuous
/// samples are generated only while the ring has room. Neither mode loops a finite buffer.
#[derive(Debug)]
pub enum Rendering {
    Baked(Baked),
    Continuous(Continuous),
}

impl Rendering {
    fn sample_rate(&self) -> u32 {
        match self {
            Self::Baked(baked) => baked.sample_rate(),
            Self::Continuous(continuous) => continuous.sample_rate(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum StartError {
    RateChanged,
    NoSlot,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Playing,
    Finished,
    Revoked,
    /// The caller must rebuild at the mixer's current rate. Queued samples are discarded
    /// with the old handle rather than continuing to feed a differently clocked device.
    RateChanged,
}

#[derive(Debug)]
pub struct Playback {
    source: Option<SourceHandle>,
    rendering: Rendering,
    cursor: usize,
    pending: [f32; 512],
    pending_start: usize,
    pending_end: usize,
    exhausted: bool,
    terminal: Option<Status>,
}

impl Playback {
    /// Claims once. A rejected one-shot is dropped, never queued for an unrelated future slot.
    pub fn start(
        mixer: &AudioMixer,
        bus: Bus,
        rendering: Rendering,
        placement: Placement,
    ) -> Result<Self, StartError> {
        if rendering.sample_rate() != mixer.0.sample_rate() {
            return Err(StartError::RateChanged);
        }
        let source = mixer.claim(bus).ok_or(StartError::NoSlot)?;
        source.require_rate(rendering.sample_rate());
        source.place(placement);
        Ok(Self {
            source: Some(source),
            rendering,
            cursor: 0,
            pending: [0.0; 512],
            pending_start: 0,
            pending_end: 0,
            exhausted: false,
            terminal: None,
        })
    }

    pub fn place(&self, placement: Placement) {
        if let Some(source) = &self.source {
            source.place(placement);
        }
    }

    /// Continuous beds fade out; baked one-shots finish their already bounded description.
    pub fn stop(&mut self) {
        if let Rendering::Continuous(continuous) = &mut self.rendering {
            continuous.stop();
        }
    }

    /// Bounded to one ring capacity per Update even if the callback consumes concurrently.
    /// The handle remains owned until the final queued zero has actually been consumed.
    pub fn pump(&mut self) -> Status {
        if let Some(status) = &self.terminal {
            return status.clone();
        }
        let Some(source) = &self.source else {
            return Status::Finished;
        };
        let terminal = if !source.live() {
            Some(Status::Revoked)
        } else if source.mixer().sample_rate() != self.rendering.sample_rate() {
            Some(Status::RateChanged)
        } else {
            None
        };
        if let Some(terminal) = terminal {
            return self.finish(terminal);
        }
        let mut budget = source.free().min(SOURCE_CAPACITY);
        while budget > 0 {
            if self.pending_start == self.pending_end {
                if self.exhausted {
                    break;
                }
                let count = budget.min(self.pending.len());
                self.pending_start = 0;
                self.pending_end = match &mut self.rendering {
                    Rendering::Baked(baked) => {
                        let count = count.min(baked.samples().len() - self.cursor);
                        self.pending[..count]
                            .copy_from_slice(&baked.samples()[self.cursor..self.cursor + count]);
                        self.cursor += count;
                        count
                    }
                    Rendering::Continuous(continuous) => {
                        continuous.render(&mut self.pending[..count])
                    }
                };
                if self.pending_end == 0 {
                    self.exhausted = true;
                    break;
                }
            }
            let end = self.pending_end.min(self.pending_start + budget);
            let count = source.push(&self.pending[self.pending_start..end]);
            self.pending_start += count;
            budget -= count;
            if count == 0 {
                break;
            }
        }
        if self.exhausted && source.free() == SOURCE_CAPACITY {
            return self.finish(Status::Finished);
        }
        Status::Playing
    }

    fn finish(&mut self, status: Status) -> Status {
        self.source = None;
        self.terminal = Some(status.clone());
        status
    }
}
