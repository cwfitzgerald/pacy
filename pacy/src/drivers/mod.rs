use std::time::Duration;

pub mod win11;

pub trait TimeDriver {
    fn get_timing_target(&self, rect: Rect) -> TimingTarget;

    fn next_presentation(&self, monitor: TimingTarget) -> FuturePresentation;
}

#[derive(Debug, Clone)]
pub struct FuturePresentation {
    /// The current time according to the presentation engine.
    ///
    /// Duration relative to an arbitrary epoch (e.g. program start).
    pub now: Duration,
    /// The soonest time from now that the display will update.
    ///
    /// Duration relative to an arbitrary epoch (e.g. program start).
    pub soonest_presentation: Duration,
    /// The latest time from now that the display will update.
    ///
    /// Duration relative to an arbitrary epoch (e.g. program start).
    pub latest_presentation: Duration,
    /// The interval between display updates.
    pub display_interval: DisplayInterval,
}

impl FuturePresentation {
    pub fn soonest_presentation_after(&self, after: Duration) -> Duration {
        if after <= self.soonest_presentation {
            return self.soonest_presentation;
        }
        let intervals = (after - self.soonest_presentation)
            .as_nanos()
            .div_ceil(self.display_interval.interval.as_nanos());
        self.soonest_presentation + self.display_interval.interval * (intervals as u32)
    }
}

#[derive(Debug, Clone)]
pub struct DisplayInterval {
    /// The interval between display updates. On VRR displays,
    /// this is the "fixed rate" interval.
    pub interval: Duration,
    /// The variable refresh rate range of the display.
    ///
    /// None if the display does not support VRR or VRR
    /// information cannot be determined.
    pub vrr_range: Option<(Duration, Duration)>,
}

// The timing target to use for presentation tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimingTarget {
    Compositor,
    Monitor(u32),
}

pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}
