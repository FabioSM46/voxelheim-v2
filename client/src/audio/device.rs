//! The output device: one supervisor thread, one `cpal::Stream`, and nothing else that
//! owns either.
//!
//! This is the half of `audio/` that touches the platform. [`mixer`](super::mixer) decides
//! what a sample is; this file decides where the samples go, and its whole job is to keep a
//! stream open across a device that errors, is unplugged, or was never there.
//!
//! ## Why a thread, and why exactly one
//!
//! A `cpal::Stream` is not `Send` on every platform, so it cannot live in a Bevy resource
//! Bevy may move between threads. It lives on a supervisor thread of its own — the shape
//! `net/session.rs` uses for the socket — and [`AudioDevice`] holds only the handle and the
//! flag that stops it. Nothing else may open a device: two owners of one output is two
//! answers to "how loud is the game", and the first thing two answers do is disagree.
//!
//! ```text
//!   Bevy (audio/mod.rs)     supervisor thread        output callback
//!   ───────────────────     ─────────────────        ───────────────
//!   AudioDevice::start ───▶ open ─▶ hold ─▶ reopen   Mixer::render
//!                    stop ─▶ Watch ◀── lose ─────────── error callback
//! ```
//!
//! ## The error callback is a real-time thread too
//!
//! `cpal` calls it from the thread it calls the data callback from, so the rule in
//! `audio/mod.rs` covers it: no allocation, no lock, no `warn!`. It stores a code in an
//! atomic and notifies a condition variable **without taking the lock**, because taking one
//! is the thing it may not do. The cost is a wakeup the supervisor can miss, which is why
//! the supervisor waits with a timeout rather than forever: a missed notification delays a
//! reopen by [`POLL_WHILE_PLAYING`] instead of losing it. [`Watch::stop`] runs on a Bevy
//! thread and *does* take the lock, so quitting is never delayed by that race.
//!
//! ## Two orderings, and why they live in the supervisor
//!
//! Both were bugs first, found in review on this file, and both are the same shape: a rule
//! that is easy to state and easy for one implementation to forget.
//!
//! - **The mixer learns the format between building the stream and starting it.** Starting
//!   is what lets the output callback run, so a callback that runs first renders buffers in
//!   the previous stream's shape — mono fanned into a stereo frame, on a reopen.
//! - **The loss code is cleared before an open attempt, never after one.** A stream can die
//!   while it is starting, and `cpal` reports a stream error *once*: clearing afterwards
//!   discards the only notification there will ever be, leaving this loop holding a dead
//!   stream and the client silent.
//!
//! [`opened`] owns both, so one copy serves every [`OutputHost`] — which is also the only
//! way a test can hold them, since the only implementation a test drives is a fake.
//!
//! ## A missing device is a log line and a silent client
//!
//! Every failure here is recoverable: no device, no format this mixer can render into, a
//! stream that will not open, a device unplugged mid-session. Each leaves the supervisor
//! waiting [`RETRY_AFTER_LOSS`] and trying again, so a device that appears later is picked
//! up without a restart. There is no `unwrap`, `expect` or `panic!` on any path a device can
//! reach — a player with no sound card plays a silent game, and the game has to keep
//! running. **The retry is a wait, not a spin**, and the log is throttled to one line every
//! [`FAILURE_LOG_EVERY`] attempts.
//!
//! ## Float only, deliberately
//!
//! The stream is opened as `f32`, so the device's own buffer *is* the [`Sink`] and the
//! callback converts nothing; an integer format would need a scratch buffer sized before the
//! stream opened, which is the allocation the rule above forbids. A device offering no float
//! configuration is reported and retried like any other failure. [`float_config`] carries
//! the rest, including which rate it asks for and why it is not the highest one.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use bevy::prelude::*;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, StreamError, SupportedStreamConfig};

use super::mixer::{Mixer, Sink};

/// How long the supervisor sleeps between looks at a stream that is playing.
///
/// It bounds two things, neither a poll of the audio itself: how long a notification the
/// error callback could not deliver takes to be noticed anyway, and how long the client
/// keeps playing to a device the system has stopped calling default.
const POLL_WHILE_PLAYING: Duration = Duration::from_millis(500);

/// How long the supervisor waits after a failure before opening again.
///
/// Long enough that a machine with no sound card spends no measurable slice of a core on
/// it, short enough that plugging a headset in is noticed while the player still holds it.
const RETRY_AFTER_LOSS: Duration = Duration::from_secs(2);

/// One failure in this many is written to the log; the rest are silent.
///
/// At [`RETRY_AFTER_LOSS`] that is one line a minute for a device that is never coming
/// back: enough to diagnose a silent client, little enough to leave in an unread log.
const FAILURE_LOG_EVERY: u32 = 30;

/// [`Watch::loss`] while the stream is healthy.
const PLAYING: u8 = 0;
/// [`Watch::loss`] when the device is gone — unplugged, or taken by something else.
const DEVICE_GONE: u8 = 1;
/// [`Watch::loss`] when the backend reported an error that is not a disappearance.
const BACKEND_ERROR: u8 = 2;
/// [`Watch::loss`] when the supervisor itself noticed the system's default device move.
const DEFAULT_MOVED: u8 = 3;

/// What a device is called when the host will not say.
const UNNAMED: &str = "an unnamed output device";

/// Which of the codes above happened, as a sentence for the log.
///
/// The error callback may not format a string, so the reason travels as a number and
/// becomes words here, on a thread allowed to spend the time.
const fn why(loss: u8) -> &'static str {
    match loss {
        DEVICE_GONE => "the device is no longer available",
        DEFAULT_MOVED => "the system's default output device changed",
        _ => "the audio backend reported an error",
    }
}

/// How long the supervisor waits in each of its two states.
///
/// A parameter rather than two constants read directly, so a test can drive the loop at a
/// pace a suite can afford while [`Pace::REAL`] stays the one thing the client runs.
#[derive(Clone, Copy, Debug)]
struct Pace {
    playing: Duration,
    after_failure: Duration,
}

impl Pace {
    /// What the client itself runs at.
    const REAL: Self = Self {
        playing: POLL_WHILE_PLAYING,
        after_failure: RETRY_AFTER_LOSS,
    };
}

/// The format an open stream negotiated, and what the device is called.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Format {
    /// `None` when the host would not name the device, which is a different thing from
    /// [`UNNAMED`]: a name nobody can read cannot be compared with anything, and treating
    /// the placeholder as one is how a working stream gets reopened forever.
    name: Option<String>,
    sample_rate: u32,
    channels: u16,
}

impl Format {
    /// The device's name for a log line.
    fn shown(&self) -> &str {
        self.name.as_deref().unwrap_or(UNNAMED)
    }
}

/// The two flags the supervisor sleeps on.
///
/// Shared with the stream's error callback, which is why every field is an atomic and the
/// notify path takes no lock. See the module doc.
#[derive(Debug, Default)]
struct Watch {
    /// [`PLAYING`], or one of the loss codes above. Written by the error callback and by
    /// the supervisor; read only by the supervisor.
    loss: AtomicU8,
    /// Set once, by [`AudioDevice`]'s `Drop`. Never cleared.
    stopping: AtomicBool,
    /// Held only to wait on [`Self::wake`]. It guards no state — every piece of state here
    /// is an atomic, precisely so the real-time side never has to hold this.
    quiet: Mutex<()>,
    wake: Condvar,
}

impl Watch {
    /// Records why the stream stopped and wakes the supervisor.
    ///
    /// **This runs on the audio thread**: one store, one notify, no lock, no allocation.
    fn lose(&self, loss: u8) {
        self.loss.store(loss, Ordering::Release);
        self.wake.notify_all();
    }

    /// The loss code, or [`PLAYING`].
    fn loss(&self) -> u8 {
        self.loss.load(Ordering::Acquire)
    }

    /// Forgets a loss, so a freshly opened stream starts healthy.
    fn playing(&self) {
        self.loss.store(PLAYING, Ordering::Release);
    }

    /// Asks the supervisor to close the stream and end.
    ///
    /// Unlike [`Self::lose`] this locks before notifying: it runs on a Bevy thread, where a
    /// lock costs nothing and a missed wakeup would delay quitting.
    fn stop(&self) {
        self.stopping.store(true, Ordering::Release);
        drop(self.quiet.lock().unwrap_or_else(PoisonError::into_inner));
        self.wake.notify_all();
    }

    fn stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    /// Waits for a notification, or `at_most`, whichever comes first.
    ///
    /// A poisoned lock is taken anyway: nothing is stored behind it, so no invariant can
    /// have been broken, and refusing to wait would turn a lock nobody reads into a spin.
    fn rest(&self, at_most: Duration) {
        let quiet = self.quiet.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = self.wake.wait_timeout(quiet, at_most);
    }
}

/// What the supervisor needs from an audio host.
///
/// **The seam that keeps every test in this file away from a sound card.** `cpal`'s own
/// traits cannot be implemented without writing a whole backend; these four methods are all
/// the loop below uses, so the fake in the tests is twenty lines.
trait OutputHost {
    /// A built stream that is not yet running. Dropping it closes the device.
    type Stream;

    /// Every output device this host can name, in the host's own order.
    fn device_names(&self) -> Vec<String>;

    /// What the host currently calls its default output device, if it has one.
    fn default_name(&self) -> Option<String>;

    /// Builds a stream on the default output device, without running it.
    fn open(
        &self,
        mixer: &Arc<Mixer>,
        watch: &Arc<Watch>,
    ) -> Result<(Self::Stream, Format), String>;

    /// Runs a stream [`Self::open`] built, so its callback begins.
    ///
    /// **Separate from `open` on purpose**, so the ordering lives in [`opened`]. See the
    /// module doc.
    fn start(&self, stream: &Self::Stream) -> Result<(), String>;
}

/// Opens a stream, tells the mixer what it negotiated, and only then runs it.
fn opened<H: OutputHost>(
    host: &H,
    mixer: &Arc<Mixer>,
    watch: &Arc<Watch>,
) -> Result<(H::Stream, Format), String> {
    let (stream, format) = host.open(mixer, watch)?;
    // The one caller of `set_format`, and before the start rather than after the open, so
    // that no buffer is ever rendered in the previous stream's shape.
    mixer.set_format(format.sample_rate, format.channels);
    host.start(&stream)?;
    Ok((stream, format))
}

/// The supervisor loop: open, hold, reopen.
///
/// Runs until [`Watch::stop`]. It never returns early on a failure, because a failure here
/// is a device that might come back.
fn supervise<H: OutputHost>(host: &H, mixer: &Arc<Mixer>, watch: &Arc<Watch>, pace: Pace) {
    // Once, at startup: what this machine has. Enumeration is not free on every backend,
    // so it happens here and on a logged failure, never on a reopen.
    //
    // **Bound before the macro rather than inside it, here and below**: `debug!` and
    // `warn!` evaluate their fields only when the callsite is enabled, so an enumeration
    // written inline would run or not depending on the log level — a diagnostic no test
    // can observe, and counting these calls is how the tests pin the throttle.
    let seen = host.device_names();
    debug!("audio output devices: {seen:?}");

    let mut failures: u32 = 0;
    while !watch.stopping() {
        // Cleared **before** the attempt and never after it: a device that fails the moment
        // `opened` starts it reports its loss while that call is still on the stack, and
        // `cpal` reports a stream error once. See the module doc.
        watch.playing();
        match opened(host, mixer, watch) {
            Ok((stream, format)) => {
                failures = 0;
                info!(
                    "audio output: {} at {} Hz, {} channel(s)",
                    format.shown(),
                    format.sample_rate,
                    format.channels
                );

                while !watch.stopping() && watch.loss() == PLAYING {
                    watch.rest(pace.playing);
                    // What the error callback cannot see: a host that moved its default
                    // without failing the stream we hold. Skipped for a device the host
                    // would not name, because `default_name` fails the same way — comparing
                    // two unknowns would reopen a working stream on every poll.
                    if let Some(name) = format.name.as_deref()
                        && host.default_name().as_deref() != Some(name)
                    {
                        watch.lose(DEFAULT_MOVED);
                    }
                }

                let loss = watch.loss();
                // Closing before anything else is attempted, so there is never a moment
                // with two streams open on one device.
                drop(stream);
                if !watch.stopping() {
                    warn!("the audio stream stopped: {}. Reopening.", why(loss));
                }
            }
            Err(err) => {
                if failures.is_multiple_of(FAILURE_LOG_EVERY) {
                    let seen = host.device_names();
                    warn!("no audio output ({err}); the client is silent. Devices seen: {seen:?}");
                }
                failures = failures.saturating_add(1);
                watch.rest(pace.after_failure);
            }
        }
    }
}

/// The supervisor thread, as the ECS holds it.
///
/// A handle and a flag. Everything that can fail happens on the other side of it, which is
/// why building one is infallible: a client that cannot start an audio thread is a silent
/// client, not one that will not start.
#[derive(Resource, Debug)]
pub struct AudioDevice {
    watch: Arc<Watch>,
    supervisor: Option<JoinHandle<()>>,
}

impl AudioDevice {
    /// Starts the supervisor on `mixer`.
    pub fn start(mixer: Arc<Mixer>) -> Self {
        let watch = Arc::new(Watch::default());
        let supervisor = thread::Builder::new()
            .name("voxelheim-audio".to_owned())
            .spawn({
                let watch = Arc::clone(&watch);
                // The host is built here rather than passed in, so nothing outside this
                // thread has to be able to hold one.
                move || supervise(&CpalHost::new(), &mixer, &watch, Pace::REAL)
            })
            .map_err(|err| warn!("the audio thread would not start ({err}); the client is silent"))
            .ok();
        Self { watch, supervisor }
    }
}

impl Drop for AudioDevice {
    /// Dropping the resource is how the app says "close the device".
    ///
    /// Joined rather than detached, because an abandoned stream outlives the window on some
    /// backends. The wait is bounded by one open attempt: [`Watch::stop`] takes the lock,
    /// so a resting supervisor cannot miss it.
    fn drop(&mut self) {
        self.watch.stop();
        if let Some(supervisor) = self.supervisor.take() {
            let _ = supervisor.join();
        }
    }
}

/// The real host.
struct CpalHost(cpal::Host);

impl CpalHost {
    fn new() -> Self {
        Self(cpal::default_host())
    }
}

impl OutputHost for CpalHost {
    type Stream = cpal::Stream;

    fn device_names(&self) -> Vec<String> {
        match self.0.output_devices() {
            Ok(devices) => named(devices.map(|device| device.name())),
            // Not an error worth failing over: the list is a diagnostic, and an empty one
            // says what a missing one would.
            Err(_) => Vec::new(),
        }
    }

    fn default_name(&self) -> Option<String> {
        self.0.default_output_device()?.name().ok()
    }

    fn open(
        &self,
        mixer: &Arc<Mixer>,
        watch: &Arc<Watch>,
    ) -> Result<(cpal::Stream, Format), String> {
        let device = self
            .0
            .default_output_device()
            .ok_or_else(|| "this host has no default output device".to_owned())?;
        let name = device.name().ok();
        let config = float_config(&device).ok_or_else(|| {
            let shown = name.as_deref().unwrap_or(UNNAMED);
            format!("{shown} offers no 32-bit float output configuration")
        })?;
        let format = Format {
            name,
            sample_rate: config.sample_rate().0,
            channels: config.channels(),
        };

        let render = Arc::clone(mixer);
        let lost = Arc::clone(watch);
        let stream = device
            .build_output_stream(
                &config.config(),
                // The output callback, and the whole of it: the device's own buffer is the
                // sink, so this allocates nothing, locks nothing and logs nothing.
                move |block: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    render.render(&mut Block(block));
                },
                // The error callback, on that same thread. A number, and a notify.
                move |err| {
                    lost.lose(match err {
                        StreamError::DeviceNotAvailable => DEVICE_GONE,
                        StreamError::BackendSpecific { .. } => BACKEND_ERROR,
                    });
                },
                None,
            )
            .map_err(|err| format!("cannot open {}: {err}", format.shown()))?;
        Ok((stream, format))
    }

    fn start(&self, stream: &cpal::Stream) -> Result<(), String> {
        stream
            .play()
            .map_err(|err| format!("cannot start the output stream: {err}"))
    }
}

/// The device's buffer, as a [`Sink`].
struct Block<'a>(&'a mut [f32]);

impl Sink for Block<'_> {
    fn block(&mut self) -> &mut [f32] {
        self.0
    }
}

/// The float configuration this device should be opened with, if it has one.
///
/// **Not `with_max_sample_rate()`, the idiom `cpal`'s own examples reach for.** That takes
/// the ceiling of whichever range it is handed: a device advertising 192 kHz would have the
/// mixer generate four times the samples — for content that is voice — and the platform
/// resample all of them back down, because the mixer the operating system runs is still at
/// its own default rate. That rate is the device's *default* configuration, so it is what is
/// asked for, with the fewest conversions between here and a speaker.
///
/// The default is used outright when it is already float, the common case; when it is not,
/// the float ranges are searched for the window closest to that same rate, ties going to
/// the configuration with fewer channels.
fn float_config(device: &cpal::Device) -> Option<SupportedStreamConfig> {
    let default = device.default_output_config().ok()?;
    if default.sample_format() == SampleFormat::F32 {
        return Some(default);
    }
    let wanted = default.sample_rate().0;
    device
        .supported_output_configs()
        .ok()?
        .filter(|range| range.sample_format() == SampleFormat::F32)
        .filter_map(|range| {
            let rate = wanted.clamp(range.min_sample_rate().0, range.max_sample_rate().0);
            // The non-panicking variant, though the clamp above is what makes it succeed:
            // `with_sample_rate` panics out of range, and nothing here may panic.
            range.try_with_sample_rate(SampleRate(rate))
        })
        .min_by_key(|config| (config.sample_rate().0.abs_diff(wanted), config.channels()))
}

/// The names a host answered with, in order, without the ones it would not name.
///
/// Generic over the error so a test can drive it without a backend. Duplicates are dropped:
/// a host may present one card under two paths, and a list offering the same name twice is
/// a list a player cannot choose from.
fn named<E>(devices: impl Iterator<Item = Result<String, E>>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in devices.flatten() {
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// **No test below opens an audio device.** Every one drives [`supervise`] through
/// [`FakeHost`]; nothing here constructs [`CpalHost`], the only code in this file that
/// reaches the platform — which is the whole reason [`OutputHost`] exists.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::mixer::{Bus, DEFAULT_SAMPLE_RATE};
    use std::collections::VecDeque;
    use std::sync::MutexGuard;
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    /// A pace no test has to wait for.
    const BRISK: Pace = Pace {
        playing: Duration::from_millis(1),
        after_failure: Duration::from_millis(1),
    };

    fn lock<T>(what: &Mutex<T>) -> MutexGuard<'_, T> {
        what.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A format with a name, so a test can say which device answered.
    fn format(name: &str, sample_rate: u32, channels: u16) -> Format {
        Format {
            name: Some(name.to_owned()),
            sample_rate,
            channels,
        }
    }

    /// A format from a host that would not say what the device is called.
    fn nameless(sample_rate: u32, channels: u16) -> Format {
        Format {
            name: None,
            sample_rate,
            channels,
        }
    }

    /// One open stream. Counting itself out on drop is how a test asserts that the old
    /// stream is closed before a new one opens.
    struct FakeStream(Arc<AtomicUsize>);

    impl Drop for FakeStream {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }

    #[derive(Default)]
    struct FakeHost {
        /// What each successive `open` answers; the last one repeats forever.
        answers: Mutex<VecDeque<Result<Format, String>>>,
        names: Vec<String>,
        default: Mutex<Option<String>>,
        opens: AtomicUsize,
        listings: AtomicUsize,
        live: Arc<AtomicUsize>,
        /// How many streams were open when each `open` was called.
        found_live: Mutex<Vec<usize>>,
        /// The mixer the supervisor drives, kept so `start` can look at it.
        mixer: Mutex<Option<Arc<Mixer>>>,
        /// The mixer's sample rate when each `start` was called — how a test sees whether
        /// the format reached the mixer before the callback could run.
        rate_at_start: Mutex<Vec<u32>>,
        /// Makes the next `open` report a loss the way an error callback would, while
        /// `open` is still on the stack.
        lose_while_opening: AtomicBool,
    }

    impl FakeHost {
        /// A host answering `answers` in turn, its default named after the first Ok one.
        fn new(answers: Vec<Result<Format, String>>) -> Arc<Self> {
            let default = answers
                .iter()
                .flatten()
                .next()
                .and_then(|format| format.name.clone());
            Arc::new(Self {
                answers: Mutex::new(answers.into()),
                names: vec!["a card".to_owned()],
                default: Mutex::new(default),
                ..Self::default()
            })
        }

        fn opens(&self) -> usize {
            self.opens.load(Ordering::Relaxed)
        }

        fn listings(&self) -> usize {
            self.listings.load(Ordering::Relaxed)
        }
    }

    impl OutputHost for FakeHost {
        type Stream = FakeStream;

        fn device_names(&self) -> Vec<String> {
            self.listings.fetch_add(1, Ordering::Relaxed);
            self.names.clone()
        }

        fn default_name(&self) -> Option<String> {
            lock(&self.default).clone()
        }

        fn open(
            &self,
            mixer: &Arc<Mixer>,
            watch: &Arc<Watch>,
        ) -> Result<(FakeStream, Format), String> {
            *lock(&self.mixer) = Some(Arc::clone(mixer));
            lock(&self.found_live).push(self.live.load(Ordering::Relaxed));
            self.opens.fetch_add(1, Ordering::Relaxed);
            if self.lose_while_opening.swap(false, Ordering::Relaxed) {
                // What a device that dies the instant it runs does: the error callback
                // fires before this call has even returned.
                watch.lose(DEVICE_GONE);
            }
            let mut answers = lock(&self.answers);
            let answer = if answers.len() > 1 {
                answers.pop_front()
            } else {
                answers.front().cloned()
            };
            drop(answers);
            match answer {
                Some(Ok(format)) => {
                    self.live.fetch_add(1, Ordering::Relaxed);
                    Ok((FakeStream(Arc::clone(&self.live)), format))
                }
                Some(Err(err)) => Err(err),
                None => Err("this host was given no answer".to_owned()),
            }
        }

        fn start(&self, _stream: &FakeStream) -> Result<(), String> {
            let rate = lock(&self.mixer)
                .as_ref()
                .map_or(0, |mixer| mixer.sample_rate());
            lock(&self.rate_at_start).push(rate);
            Ok(())
        }
    }

    /// A supervisor on its own thread, with the mixer and watch it drives.
    struct Running {
        mixer: Arc<Mixer>,
        watch: Arc<Watch>,
        thread: Option<JoinHandle<()>>,
    }

    impl Running {
        fn start(host: &Arc<FakeHost>, pace: Pace) -> Self {
            let mixer = Arc::new(Mixer::new());
            let watch = Arc::new(Watch::default());
            let thread = thread::spawn({
                let host = Arc::clone(host);
                let mixer = Arc::clone(&mixer);
                let watch = Arc::clone(&watch);
                move || supervise(&*host, &mixer, &watch, pace)
            });
            Self {
                mixer,
                watch,
                thread: Some(thread),
            }
        }

        /// Stops the supervisor and answers whether it ended without panicking.
        fn stop(&mut self) -> bool {
            self.watch.stop();
            self.thread
                .take()
                .is_none_or(|thread| thread.join().is_ok())
        }
    }

    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.stop();
        }
    }

    /// Waits up to five seconds for `ready`, and answers whether it came true.
    ///
    /// Generous on purpose: every assertion built on it fails in the direction of a slow
    /// machine taking longer, never of a fast one seeing more than it should.
    fn until(mut ready: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if ready() {
                return true;
            }
            thread::sleep(Duration::from_millis(2));
        }
        ready()
    }

    #[test]
    fn the_mixer_has_the_format_before_the_callback_can_run() {
        // 44_100 rather than the mixer's own default, so "was told" and "was never told"
        // are different numbers rather than the same one.
        let host = FakeHost::new(vec![Ok(format("a card", 44_100, 2))]);
        let mut running = Running::start(&host, BRISK);
        assert!(until(|| !lock(&host.rate_at_start).is_empty()));

        // The channel count reached it too, which has no getter: two samples out per one
        // sample in is what a stereo stream means.
        let source = running.mixer.claim(Bus::Master).expect("a free slot");
        running.mixer.set_gain(Bus::Master, 1.0);
        source.push(&[0.5]);
        let mut block = [0.0_f32; 2];
        running.mixer.render(&mut Block(&mut block));
        assert_eq!(block, [0.5, 0.5]);

        assert!(running.stop(), "the supervisor ended cleanly");
        assert_eq!(
            *lock(&host.rate_at_start),
            vec![44_100],
            "the mixer knew the buffer shape before the callback could run"
        );
        assert_eq!(
            host.live.load(Ordering::Relaxed),
            0,
            "the stream was closed"
        );
    }

    #[test]
    fn a_lost_stream_is_reopened_and_never_while_the_old_one_is_open() {
        let host = FakeHost::new(vec![
            Ok(format("a card", 44_100, 2)),
            Ok(format("a card", 48_000, 1)),
        ]);
        let mut running = Running::start(&host, BRISK);
        assert!(until(|| running.mixer.sample_rate() == 44_100));

        // What the error callback does, and the whole of what it does.
        running.watch.lose(DEVICE_GONE);

        assert!(until(|| running.mixer.sample_rate() == 48_000), "reopened");
        assert!(running.stop());
        assert!(host.opens() >= 2);
        assert!(
            lock(&host.found_live).iter().all(|live| *live == 0),
            "every open found the previous stream already closed"
        );
    }

    #[test]
    fn a_host_with_no_device_is_retried_rather_than_panicking() {
        let host = FakeHost::new(vec![Err(
            "this host has no default output device".to_owned()
        )]);
        let mut running = Running::start(&host, BRISK);

        assert!(until(|| host.opens() >= 3), "it keeps trying");
        assert_eq!(
            running.mixer.sample_rate(),
            DEFAULT_SAMPLE_RATE,
            "a device that never opened told the mixer nothing"
        );
        assert!(running.stop(), "nothing panicked, and stopping still works");
    }

    #[test]
    fn a_failing_open_waits_rather_than_spinning() {
        let host = FakeHost::new(vec![Err("nothing here".to_owned())]);
        let mut running = Running::start(
            &host,
            Pace {
                playing: Duration::from_millis(1),
                after_failure: Duration::from_secs(2),
            },
        );

        assert!(until(|| host.opens() >= 1));
        thread::sleep(Duration::from_millis(60));
        assert_eq!(
            host.opens(),
            1,
            "one attempt, then a wait — a retry loop that spun would have made hundreds"
        );
        // And the wait is interruptible: stopping does not have to outlast it.
        let stopped = Instant::now();
        assert!(running.stop());
        assert!(stopped.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_permanently_absent_device_does_not_write_a_log_line_per_attempt() {
        let host = FakeHost::new(vec![Err("nothing here".to_owned())]);
        let mut running = Running::start(&host, BRISK);

        // The enumeration runs once at startup and once per *logged* failure, and it runs
        // whether or not a subscriber is listening — see `supervise`. Counting listings is
        // therefore counting log lines, which is the only handle a unit test has on them.
        assert!(until(|| host.opens() >= (FAILURE_LOG_EVERY as usize) * 3));
        let (opens, listings) = (host.opens(), host.listings());
        assert!(running.stop());
        assert!(
            listings <= opens / (FAILURE_LOG_EVERY as usize) + 2,
            "{listings} log lines for {opens} attempts is not a throttle"
        );
        assert!(listings >= 2, "and it is not silence either");
    }

    #[test]
    fn the_default_output_device_moving_reopens_the_stream() {
        let host = FakeHost::new(vec![
            Ok(format("the speakers", 48_000, 2)),
            Ok(format("the headset", 44_100, 2)),
        ]);
        let mut running = Running::start(&host, BRISK);
        assert!(until(|| running.mixer.sample_rate() == 48_000));

        // Nothing failed: the host simply started calling something else its default —
        // the case no error callback reports.
        *lock(&host.default) = Some("the headset".to_owned());

        assert!(
            until(|| running.mixer.sample_rate() == 44_100),
            "the client followed the system's default"
        );
        assert!(running.stop());
    }

    #[test]
    fn a_stream_that_dies_while_it_is_opening_is_reopened_rather_than_held() {
        // Both answers name the same device, so nothing here can be reopened by the
        // default-moved poll: a reopen can only come from the loss surviving.
        let host = FakeHost::new(vec![
            Ok(format("a card", 44_100, 2)),
            Ok(format("a card", 22_050, 2)),
        ]);
        host.lose_while_opening.store(true, Ordering::Relaxed);
        let mut running = Running::start(&host, BRISK);

        assert!(
            until(|| running.mixer.sample_rate() == 22_050),
            "the loss stored during the open survived into the inner loop"
        );
        assert!(running.stop());
    }

    #[test]
    fn a_device_the_host_will_not_name_is_left_alone_rather_than_reopened() {
        let host = FakeHost::new(vec![Ok(nameless(44_100, 2))]);
        let mut running = Running::start(&host, BRISK);
        assert!(until(|| running.mixer.sample_rate() == 44_100));

        // `default_name` fails the way `name` did, so an unguarded comparison would find
        // them unequal on every poll — fifty of them at this pace.
        thread::sleep(Duration::from_millis(50));
        let opens = host.opens();
        assert!(running.stop());
        assert_eq!(opens, 1, "a working stream was reopened {opens} times");
    }

    #[test]
    fn a_name_the_host_will_not_give_is_skipped_and_a_duplicate_appears_once() {
        let answers: Vec<Result<String, ()>> = vec![
            Ok("HDA Intel".to_owned()),
            Err(()),
            Ok("HDA Intel".to_owned()),
            Ok(String::new()),
            Ok("USB headset".to_owned()),
        ];

        assert_eq!(
            named(answers.into_iter()),
            vec!["HDA Intel".to_owned(), "USB headset".to_owned()],
            "unreadable, duplicate and empty names are all dropped"
        );
        assert!(named(std::iter::empty::<Result<String, ()>>()).is_empty());
    }
}
