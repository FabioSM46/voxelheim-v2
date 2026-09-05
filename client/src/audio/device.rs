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

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use bevy::prelude::*;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, StreamError, SupportedStreamConfig};

use super::mixer::{Mixer, Ring, Sink};

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
/// [`Watch::loss`] when the player picked a different output device in the settings.
const CHOICE_CHANGED: u8 = 4;

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
        CHOICE_CHANGED => "a different output device was chosen in the settings",
        _ => "the audio backend reported an error",
    }
}

/// What a supervisor does with a stream that stopped: name it, and pace what comes next.
///
/// **`held` is the whole of the distinction, and without it both loops spin.** A stream that
/// ran and then failed is reopened at once, because a device that was working a moment ago
/// probably still exists. A stream that never ran — the error callback fired while `open` was
/// still on the stack, which is exactly the case the loss code is cleared early for — leaves
/// the hold loop's body unexecuted, so nothing has waited and nothing has been throttled.
/// Measured at [`Pace::REAL`] before this existed: 456 654 output reopens and 588 591 capture
/// reopens in 200 ms, under a module doc claiming that the retry is a wait and that the log is
/// throttled. Found by review on #919.
fn stopped(what: &str, loss: u8, held: bool, failures: &mut u32, watch: &Watch, pace: Pace) {
    if held {
        *failures = 0;
        warn!("{what} stopped: {}. Reopening.", why(loss));
        return;
    }
    if failures.is_multiple_of(FAILURE_LOG_EVERY) {
        warn!("{what} will not stay open: {}. Retrying.", why(loss));
    }
    *failures = failures.saturating_add(1);
    watch.rest(pace.after_failure);
}

/// Which device the player asked for, and which ones the host named.
///
/// **The settings tab's whole view of the device**, and the only state crossing between a
/// Bevy thread and the supervisor once a stream is open. No audio callback ever touches it —
/// the supervisor reads it between waits, Bevy on its own schedule — so the mutexes cost the
/// real-time rule nothing. [`Watch`] is still all the callbacks share. [`Self::listings`] is
/// a counter rather than a flag because Bevy asks "is this list new" every frame, and
/// cloning a `Vec<String>` under a lock to hear "no" is the wrong shape for that question.
#[derive(Debug, Default)]
struct Choice {
    /// The device the player picked, under the name its host gives it, or `None` for
    /// "follow whatever the system calls its default".
    wanted: Mutex<Option<String>>,
    /// Every output device the host named, as of the last enumeration.
    seen: Mutex<Vec<String>>,
    /// Bumped whenever [`Self::seen`] is replaced.
    listings: AtomicU64,
}

impl Choice {
    fn wanted(&self) -> Option<String> {
        held(&self.wanted).clone()
    }

    /// Asks for `name`, or for the system default when `None`. The supervisor notices within
    /// one [`Pace::playing`] rather than being woken: half a second is under the time it
    /// takes a player to look up from the mouse they just decided with.
    fn want(&self, name: Option<String>) {
        *held(&self.wanted) = name;
    }

    /// Records what the host answered, for the knob to offer.
    fn publish(&self, names: Vec<String>) {
        *held(&self.seen) = names;
        self.listings.fetch_add(1, Ordering::Release);
    }

    fn seen(&self) -> Vec<String> {
        held(&self.seen).clone()
    }

    fn listings(&self) -> u64 {
        self.listings.load(Ordering::Acquire)
    }
}

/// A lock taken even when it is poisoned — [`Watch::rest`]'s judgement, for the same reason:
/// neither field behind these holds an invariant a panicking thread could have broken, and
/// refusing would leave the settings tab with no device list for the session.
fn held<T>(what: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    what.lock().unwrap_or_else(PoisonError::into_inner)
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

    /// Wakes the supervisor without asking it to stop.
    ///
    /// [`Self::stop`]'s shape rather than [`Self::lose`]'s: it runs on a Bevy thread, where
    /// taking the lock costs nothing and a missed wakeup would cost a poll interval. Nothing
    /// in an audio callback may call this.
    fn nudge(&self) {
        drop(self.quiet.lock().unwrap_or_else(PoisonError::into_inner));
        self.wake.notify_all();
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

    /// Builds a stream on `wanted` — or on the host's default output device when it is
    /// `None` — without running it.
    ///
    /// **A named device that is not present is an error, never a fallback to the default.**
    /// A player who picked a headset and hears the speakers has been told something untrue
    /// about where the sound is going; the failure path retries, so the headset is picked up
    /// again the moment it is plugged back in.
    fn open(
        &self,
        wanted: Option<&str>,
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
    wanted: Option<&str>,
    mixer: &Arc<Mixer>,
    watch: &Arc<Watch>,
) -> Result<(H::Stream, Format), String> {
    let (stream, format) = host.open(wanted, mixer, watch)?;
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
fn supervise<H: OutputHost>(
    host: &H,
    mixer: &Arc<Mixer>,
    watch: &Arc<Watch>,
    choice: &Arc<Choice>,
    pace: Pace,
) {
    // Once, at startup: what this machine has. Enumeration is not free on every backend,
    // so it happens here, on a stream that opened, and on a logged failure, never on a
    // reopen.
    //
    // **Bound before the macro rather than inside it, here and below**: `debug!` and
    // `warn!` evaluate their fields only when the callsite is enabled, so an enumeration
    // written inline would run or not depending on the log level — a diagnostic no test
    // can observe, and counting these calls is how the tests pin the throttle.
    let seen = host.device_names();
    debug!("audio output devices: {seen:?}");

    let mut failures: u32 = 0;
    while !watch.stopping() {
        let wanted = choice.wanted();
        // Cleared **before** the attempt and never after it: a device that fails the moment
        // `opened` starts it reports its loss while that call is still on the stack, and
        // `cpal` reports a stream error once. See the module doc.
        watch.playing();
        match opened(host, wanted.as_deref(), mixer, watch) {
            Ok((stream, format)) => {
                // Not reset here any more: a stream that never runs must keep counting, or the
                // throttle in `stopped` never engages. It is cleared by a stream that held.
                if failures == 0 {
                    info!(
                        "audio output: {} at {} Hz, {} channel(s)",
                        format.shown(),
                        format.sample_rate,
                        format.channels
                    );
                }
                // Refreshed on an open rather than on a poll: an open is where this
                // machine's devices most recently changed, and is rare enough to enumerate.
                choice.publish(host.device_names());

                let mut held = false;
                while !watch.stopping() && watch.loss() == PLAYING {
                    watch.rest(pace.playing);
                    held = true;
                    // Two things the error callback cannot see. The first is the player
                    // choosing a different device.
                    if choice.wanted() != wanted {
                        watch.lose(CHOICE_CHANGED);
                    // The second is a host that moved its default without failing the
                    // stream we hold — only a reason to reopen while the player follows that
                    // default. Skipped for a device the host would not name, because
                    // `default_name` fails the same way and comparing two unknowns would
                    // reopen a working stream on every poll.
                    } else if wanted.is_none()
                        && let Some(name) = format.name.as_deref()
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
                    stopped("the audio stream", loss, held, &mut failures, watch, pace);
                }
            }
            Err(err) => {
                if failures.is_multiple_of(FAILURE_LOG_EVERY) {
                    let seen = host.device_names();
                    warn!("no audio output ({err}); the client is silent. Devices seen: {seen:?}");
                    // The one place the list is refreshed while nothing opens, so a device
                    // chosen and then unplugged still leaves the knob offering what is
                    // actually attached.
                    choice.publish(seen);
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
    choice: Arc<Choice>,
    supervisor: Option<JoinHandle<()>>,
}

impl AudioDevice {
    /// Starts the supervisor on `mixer`.
    pub fn start(mixer: Arc<Mixer>) -> Self {
        let watch = Arc::new(Watch::default());
        let choice = Arc::new(Choice::default());
        let supervisor = thread::Builder::new()
            .name("voxelheim-audio".to_owned())
            .spawn({
                let watch = Arc::clone(&watch);
                let choice = Arc::clone(&choice);
                // The host is built here rather than passed in, so nothing outside this
                // thread has to be able to hold one.
                move || supervise(&CpalHost::new(), &mixer, &watch, &choice, Pace::REAL)
            })
            .map_err(|err| warn!("the audio thread would not start ({err}); the client is silent"))
            .ok();
        Self {
            watch,
            choice,
            supervisor,
        }
    }

    /// Asks for the device called `name`, or for the system default when `None`.
    ///
    /// Recorded rather than acted on: the supervisor owns the stream and reopens within one
    /// poll. Asking for what is already wanted changes nothing, which is what lets the Bevy
    /// side write it on every settings change without thinking about it.
    pub fn use_output(&self, name: Option<String>) {
        self.choice.want(name);
    }

    /// Every output device the host named, as of the supervisor's last enumeration.
    pub fn output_devices(&self) -> Vec<String> {
        self.choice.seen()
    }

    /// How many times that list has been replaced, so a Bevy system can tell "nothing new"
    /// from "the same list again" without a lock or a clone every frame.
    pub fn listings(&self) -> u64 {
        self.choice.listings()
    }

    /// An `AudioDevice` with no supervisor, and therefore no device anywhere.
    ///
    /// **Test-only, and what keeps `audio/mod.rs`'s systems testable**: everything they say
    /// to this resource goes through [`Choice`], and [`Self::start`] is the one function
    /// that spawns the thread which would open a stream.
    #[cfg(test)]
    pub(super) fn idle() -> Self {
        Self {
            watch: Arc::new(Watch::default()),
            choice: Arc::new(Choice::default()),
            supervisor: None,
        }
    }

    /// Publishes `names` as though the supervisor had just enumerated them.
    #[cfg(test)]
    pub(super) fn enumerated(&self, names: Vec<String>) {
        self.choice.publish(names);
    }

    /// What [`Self::use_output`] last recorded.
    #[cfg(test)]
    pub(super) fn wanted(&self) -> Option<String> {
        self.choice.wanted()
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

    /// The device this host calls `wanted`, if it has one. Matched on the exact name the
    /// host gave, because that is what the file holds and what [`named`] offered the player.
    /// No fuzzy match: two cards whose names differ by a space are two cards.
    fn named_device(&self, wanted: &str) -> Option<cpal::Device> {
        self.0
            .output_devices()
            .ok()?
            .find(|device| device.name().is_ok_and(|name| name == wanted))
    }

    /// [`Self::named_device`] on the other side of the card, matched the same way and for the
    /// same reason: two cards whose names differ by a space are two cards.
    fn named_input_device(&self, wanted: &str) -> Option<cpal::Device> {
        self.0
            .input_devices()
            .ok()?
            .find(|device| device.name().is_ok_and(|name| name == wanted))
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
        wanted: Option<&str>,
        mixer: &Arc<Mixer>,
        watch: &Arc<Watch>,
    ) -> Result<(cpal::Stream, Format), String> {
        let device = match wanted {
            Some(name) => self
                .named_device(name)
                .ok_or_else(|| format!("{name} is not one of this host's output devices"))?,
            None => self
                .0
                .default_output_device()
                .ok_or_else(|| "this host has no default output device".to_owned())?,
        };
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

// -----------------------------------------------------------------------------
// The microphone. Same shape as everything above it, pointed the other way.
// -----------------------------------------------------------------------------

/// How many samples the capture ring holds.
///
/// **Samples, not milliseconds**, so how long it holds depends on the stream: a quarter of a
/// second of mono at 48 kHz, 136 ms of stereo at 44.1 kHz, 62 ms of stereo at 96 kHz. The
/// shortest is what the number has to be sized from, and the consumer is a Bevy system at the
/// frame rate — so what this survives is a *stall*, not a steady rate mismatch. At 60 Hz the
/// ring is emptied every 17 ms, and 62 ms is three of those.
const CAPTURE_CAPACITY: usize = 12_000;

/// [`Capture::fault`] while there is nothing wrong.
const NO_FAULT: u8 = 0;

/// Why the microphone is not open.
///
/// **Two causes and not five, because a player can act on two.** A device the host does not
/// list is one they can plug in or replace in the settings; anything else — busy, no float
/// configuration, a stream that would not start, a host with no default input at all — is one
/// they cannot tell apart from the outside and this client must not guess between. The log
/// carries the platform's own words for those; the screen says only that it did not work.
///
/// **The reason this is an enum rather than a flag** is that the flag it replaces was
/// documented as "the microphone the player named would not open" and set for every failure,
/// so a busy device was reported as a missing one — which sends a player to look for a cable
/// instead of closing the other application. Found by review on #928, and it is the same shape
/// as the diagnostic failure that helped reverse the substituting design two parts earlier: a
/// message right about the state and wrong about the cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureFault {
    /// The player named a device and the host does not list it.
    NotAttached,
    /// A device was there and would not open. Which of the several ways is in the log.
    WouldNotOpen,
}

impl CaptureFault {
    /// This fault as it is stored, never [`NO_FAULT`].
    const fn code(self) -> u8 {
        match self {
            Self::NotAttached => 1,
            Self::WouldNotOpen => 2,
        }
    }

    /// The fault `code` names, or `None` for [`NO_FAULT`].
    ///
    /// Total rather than fallible, and no wildcard on the way in: only [`Self::code`] writes
    /// these, so an unknown value is unreachable — and answering it as "nothing is wrong"
    /// would be the one direction this must not fail in.
    const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::NotAttached),
            NO_FAULT => None,
            _ => Some(Self::WouldNotOpen),
        }
    }
}

/// The microphone, as everything above the platform sees it.
///
/// **The mirror of [`Mixer`], and deliberately so.** A callback the operating system will not
/// wait for writes into a lock-free ring; a Bevy system reads it. What crosses between them is
/// samples and four atomics, and the rule from `audio/mod.rs` is the same one, because it is
/// the same kind of thread: no allocation, no lock, no logging, no Bevy type.
///
/// **The samples are the device's own**, interleaved at whatever rate and channel count it
/// negotiated. Resampling them to 48 kHz mono is `audio/dsp.rs`'s job and it happens on the
/// Bevy side, because a resampler carries state and a callback may not allocate the buffers
/// one needs. [`Self::format`] is what a consumer reads to build the right one, and
/// [`Self::generation`] is how it learns that the answer changed under it.
#[derive(Debug)]
pub struct Capture {
    samples: Ring,
    /// The open stream's rate, channel count and identity, in **one** word.
    ///
    /// **Three atomics would not be one answer, and the review on #919 is where that was
    /// found.** Written separately, a consumer can read the new rate beside the old
    /// generation — precisely the observation the generation exists to make impossible. The
    /// three are one fact about one stream, so they are one atomic: 32 bits of rate, 16 of
    /// channels, 16 of generation. See [`Self::pack`].
    stream: AtomicU64,
    /// Which stream the consumer has read from. **Written only by the consumer**, which is
    /// what keeps [`Ring`]'s single-consumer assumption true — see [`Self::take`].
    read_from: AtomicU32,
    /// How many samples the callback has had to drop for a full ring. A diagnostic, never a
    /// decision.
    overruns: AtomicU64,
    /// Why the microphone is not open, when it is not — a [`CaptureFault`] as `u8`, or
    /// [`NO_FAULT`].
    ///
    /// **Presentation, and the HUD is its only reader.** Nothing decides anything from it —
    /// the rule `client/AGENTS.md` states for everything under `audio/`. It is what lets the
    /// indicator say why a held key is producing nothing, which is the whole of what makes
    /// refusing a named device that is not there safe to do.
    fault: AtomicU8,
    /// Whether anything above wants a stream open at all.
    ///
    /// **A microphone nobody asked for is never opened**, which is the whole of what
    /// `VoiceMode::Off` means and the whole of what a server relaying no voice means. The
    /// supervisor holds no device while this is false.
    wanted: AtomicBool,
    /// Makes the next [`Self::take`] observe a stream opening at the one moment no fixture
    /// can otherwise put one: **between its two generation reads**.
    ///
    /// **A `cfg(test)` seam, on `Transport::Plaintext`'s precedent**, and it is here because
    /// the alternatives were both worse. The branch it reaches is unreachable from any
    /// sequential test — every reopen a reader can see *before* it starts is caught by the
    /// skip above — and a concurrent fixture reaches it only sometimes: measured, a stress
    /// loop caught a deliberately removed guard in one run of three, and tuning it either way
    /// took the detection rate to zero. A guard nobody has watched fire is a guard nobody
    /// knows the shape of, so the fixture places the reopen exactly.
    #[cfg(test)]
    reopen_mid_read: AtomicBool,
}

/// What one read of the capture ring produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Captured {
    /// The rate and channel count the samples are in.
    pub sample_rate: u32,
    pub channels: u16,
    /// **A stream this reader had not read from before**, so nothing was appended and
    /// whatever the caller was carrying — a resampler's tail, a part-built frame — belongs to
    /// a stream that has ended. See [`Capture::take`].
    pub fresh: bool,
}

impl Default for Capture {
    fn default() -> Self {
        Self::new()
    }
}

impl Capture {
    pub fn new() -> Self {
        Self {
            samples: Ring::new(CAPTURE_CAPACITY),
            stream: AtomicU64::new(0),
            read_from: AtomicU32::new(0),
            overruns: AtomicU64::new(0),
            fault: AtomicU8::new(NO_FAULT),
            wanted: AtomicBool::new(false),
            #[cfg(test)]
            reopen_mid_read: AtomicBool::new(false),
        }
    }

    /// The three fields as one word: rate, then channels, then generation.
    ///
    /// The generation wraps every 65 536 streams and skips zero, so a packed word is zero if
    /// and only if no stream has ever opened. Nothing compares two generations for order —
    /// only for equality — so wrapping costs nothing.
    const fn pack(sample_rate: u32, channels: u16, generation: u16) -> u64 {
        (sample_rate as u64) | ((channels as u64) << 32) | ((generation as u64) << 48)
    }

    /// Records what a stream negotiated and marks it a new one.
    ///
    /// One store, so there is no window in which half of it is visible. Called between
    /// building a stream and starting it, which is [`opened`]'s ordering and its reason: a
    /// callback that ran first would fill the ring with samples described by the previous
    /// stream's rate.
    fn opened_at(&self, sample_rate: u32, channels: u16) {
        let previous = (self.stream.load(Ordering::Relaxed) >> 48) as u16;
        let generation = match previous.wrapping_add(1) {
            0 => 1,
            next => next,
        };
        self.stream.store(
            Self::pack(sample_rate.max(1), channels.max(1), generation),
            Ordering::Release,
        );
    }

    /// The rate, channel count and identity of the open stream, or `None` before one has
    /// opened. One atomic load, so the three are never a mixture of two streams.
    ///
    /// Test-only: what a consumer uses is [`Self::take`], which answers the same format
    /// beside the samples it belongs to rather than as a separate question.
    #[cfg(test)]
    fn format(&self) -> Option<(u32, u16, u16)> {
        let packed = self.stream.load(Ordering::Acquire);
        if packed == 0 {
            return None;
        }
        Some((packed as u32, (packed >> 32) as u16, (packed >> 48) as u16))
    }

    /// Appends everything the **current** stream has captured to `out`.
    ///
    /// **This is the whole consumer-side contract, and it is a method rather than a rule
    /// because the review on #919 found what the rule costs.** A ring drained naively across
    /// a reopen hands back the tail of one stream stitched onto the head of the next, at two
    /// different sample rates, with nothing saying where the seam is — which is the
    /// continuation the generation exists to preclude, arriving by the other door.
    ///
    /// Two things make that unreachable:
    ///
    /// - **A stream this reader has not read from before is skipped, not appended.** What is
    ///   waiting then is the previous stream's tail plus however much of the new one has
    ///   arrived since, and nothing can tell them apart — so both go, and the answer says
    ///   [`Captured::fresh`] so the caller throws its own carried state away too. A reopen is
    ///   a gap; the skip costs a few milliseconds inside one.
    /// - **A stream that opens *while* this is reading invalidates the batch.** The
    ///   generation is read again afterwards and `out` is put back exactly as it was;
    ///   `None` is the answer, and the next call reports a fresh stream.
    ///
    /// **One consumer, which is why its position lives here.** [`Ring`] is single-consumer
    /// and `read_from` is written by nobody else — the supervisor never touches the ring, so
    /// no third party races the read index.
    // The caller is the capture pipeline in #852 part 6, the first thing with a use for a
    // captured sample. See `audio/dsp.rs` for why the seam is here.
    #[allow(dead_code)]
    pub fn take(&self, out: &mut Vec<f32>) -> Option<Captured> {
        let before = self.stream.load(Ordering::Acquire);
        if before == 0 {
            return None;
        }
        let at = out.len();

        // **One check for both paths, at one exit, and the review on #921 is why.** The first
        // version put the re-read on the draining path only — the protection was understood
        // and applied to one of two siblings, which is the shape that survives a review most
        // easily. A rule of the form "remember to re-read the generation" would have the same
        // fate at the third path, so there is no third place to put it: whichever branch runs,
        // control reaches the comparison below before anything is returned.
        let fresh =
            self.read_from.swap((before >> 48) as u32, Ordering::AcqRel) != (before >> 48) as u32;
        if fresh {
            self.samples.skip();
        } else {
            self.samples.drain_into(out);
        }
        #[cfg(test)]
        if self.reopen_mid_read.swap(false, Ordering::Relaxed) {
            self.opened_at(before as u32, (before >> 32) as u16);
        }

        // The whole word, not the generation: a stream that moved changed its rate too, and
        // an answer built from `before` would describe the one that ended.
        if self.stream.load(Ordering::Acquire) != before {
            out.truncate(at);
            // Zero is never a live generation, so the next call answers `fresh` — and it
            // answers it from the word that is live then.
            self.read_from.store(0, Ordering::Release);
            return None;
        }
        Some(Captured {
            sample_rate: before as u32,
            channels: (before >> 32) as u16,
            fresh,
        })
    }

    /// How many samples a full ring has cost. A diagnostic; nothing decides from it.
    // Read by the capture pipeline in #852 part 6, which is the first thing with a use for
    // a captured sample. See `audio/dsp.rs` for why the seam is here.
    #[allow(dead_code)]
    pub fn overruns(&self) -> u64 {
        self.overruns.load(Ordering::Relaxed)
    }

    /// Why a microphone that was asked for is not open, or `None` while one is — or while
    /// nobody has asked.
    pub fn fault(&self) -> Option<CaptureFault> {
        CaptureFault::from_code(self.fault.load(Ordering::Acquire))
    }

    /// Records what the supervisor's last open attempt did. Its one caller is that loop.
    fn set_fault(&self, fault: Option<CaptureFault>) {
        self.fault.store(
            fault.map_or(NO_FAULT, CaptureFault::code),
            Ordering::Release,
        );
    }

    /// Asks for a stream to be open, or for the one that is open to be closed.
    ///
    /// **Recorded here and woken by [`AudioCapture::listen`]**, which is the entry a caller
    /// uses. The output side can afford to be noticed within one [`Pace::playing`] — half a
    /// second is under the time it takes a player to look up from the mouse they just decided
    /// with — but the first push-to-talk press is not that: waiting out a poll would lose half
    /// a second of the first thing anybody says. Found by review on #919.
    // Read by the capture pipeline in #852 part 6, which is the first thing with a use for
    // a captured sample. See `audio/dsp.rs` for why the seam is here.
    #[allow(dead_code)]
    pub fn listen(&self, wanted: bool) {
        self.wanted.store(wanted, Ordering::Release);
    }

    /// Whether a stream is wanted.
    pub fn wanted(&self) -> bool {
        self.wanted.load(Ordering::Acquire)
    }

    /// **The whole of what runs in the capture callback.** One push into the ring, and a
    /// counter when it will not all fit. No allocation, no lock, no logging, no Bevy type —
    /// `audio/mod.rs`'s rule, on the other real-time thread.
    ///
    /// A full ring drops the newest samples rather than overwriting the oldest, which is
    /// [`Ring::push`]'s behaviour and the right one here: the consumer is mid-frame, and
    /// tearing the audio it is about to encode is worse than losing the tail of a block it
    /// has not seen.
    fn captured(&self, block: &[f32]) {
        let taken = self.samples.push(block);
        if taken < block.len() {
            self.overruns
                .fetch_add((block.len() - taken) as u64, Ordering::Relaxed);
        }
    }
}

/// What the capture supervisor needs from an audio host.
///
/// [`OutputHost`]'s counterpart, and shorter by one method: **this side has no default-moved
/// check** — see `client/AGENTS.md` for why a microphone does not follow a moved default.
trait InputHost {
    /// A built stream that is not yet running. Dropping it closes the device.
    type Stream;

    /// Every input device this host can name, in the host's own order.
    fn device_names(&self) -> Vec<String>;

    /// Builds a stream on `wanted` — or on the host's default input device when it is
    /// `None` — without running it.
    ///
    /// **A named device that is not present is an error here too.** The substitution the
    /// acceptance criterion asks for is [`supervise_capture`]'s and happens one level up,
    /// where it can be said out loud; a host that quietly opened something else would make
    /// the fallback unobservable and untestable.
    fn open(
        &self,
        wanted: Option<&str>,
        capture: &Arc<Capture>,
        watch: &Arc<Watch>,
    ) -> Result<(Self::Stream, Format), String>;

    /// Runs a stream [`Self::open`] built, so its callback begins.
    fn start(&self, stream: &Self::Stream) -> Result<(), String>;
}

/// Opens a capture stream, records what it negotiated, and only then runs it.
///
/// [`opened`]'s ordering for the other direction and for the same reason: starting is what
/// lets the callback run, and a consumer that read the previous stream's rate would resample
/// by the wrong ratio for as long as it took to notice.
fn opened_capture<H: InputHost>(
    host: &H,
    wanted: Option<&str>,
    capture: &Arc<Capture>,
    watch: &Arc<Watch>,
) -> Result<(H::Stream, Format), String> {
    let (stream, format) = host.open(wanted, capture, watch)?;
    capture.opened_at(format.sample_rate, format.channels);
    host.start(&stream)?;
    Ok((stream, format))
}

/// The capture supervisor: wait, open, hold, close, wait.
///
/// [`supervise`] with one state more. The output stream is open whenever a device will have
/// it, because a client with sound is always potentially making some; a microphone is open
/// only while something above has asked for one, because an open microphone nobody asked for
/// is not a thing this client may have. So this loop starts and ends in a wait.
fn supervise_capture<H: InputHost>(
    host: &H,
    capture: &Arc<Capture>,
    watch: &Arc<Watch>,
    choice: &Arc<Choice>,
    pace: Pace,
) {
    // Once, at startup, exactly as [`supervise`] does it and for the same reason: enumeration
    // is not free on every backend, and a knob whose bound was refreshed on every poll would
    // pay for it while nobody was looking at the settings screen. Bound before the macro,
    // because `debug!` evaluates its fields only when the callsite is enabled.
    let seen = host.device_names();
    debug!("audio input devices: {seen:?}");
    choice.publish(seen);

    let mut failures: u32 = 0;
    while !watch.stopping() {
        if !capture.wanted() {
            // Nothing is open and nothing is being held: this is the ordinary state of a
            // client whose player has not pressed the key. Nothing is wrong either — a device
            // nobody has asked for cannot have failed to open.
            capture.set_fault(None);
            watch.rest(pace.playing);
            continue;
        }

        let wanted = choice.wanted();

        // Cleared before the attempt and never after it, for the reason `supervise` states:
        // a stream that fails while it is starting reports its loss while this call is still
        // on the stack, and `cpal` reports a stream error once.
        watch.playing();
        match opened_capture(host, wanted.as_deref(), capture, watch) {
            Ok((stream, format)) => {
                // A stream opened, so whatever the player named is there and works.
                capture.set_fault(None);
                // See the output supervisor: cleared by a stream that held, not by one that
                // opened, so a stream that dies on every attempt is still throttled.
                if failures == 0 {
                    info!(
                        "voice capture: {} at {} Hz, {} channel(s)",
                        format.shown(),
                        format.sample_rate,
                        format.channels
                    );
                }
                // Refreshed on an open rather than on a poll, for [`supervise`]'s reason: an
                // open is where this machine's devices most recently changed. On this side it
                // is also the *only* time the list moves while a client is running, because a
                // microphone is opened when somebody decides to speak — which is exactly when
                // they have just plugged one in.
                choice.publish(host.device_names());

                let mut held = false;
                while !watch.stopping() && watch.loss() == PLAYING && capture.wanted() {
                    watch.rest(pace.playing);
                    held = true;
                    // The one thing the error callback cannot see: the player choosing a
                    // different microphone. There is no `DEFAULT_MOVED` twin — see the trait
                    // above and `client/AGENTS.md`.
                    if choice.wanted() != wanted {
                        watch.lose(CHOICE_CHANGED);
                    }
                }
                let loss = watch.loss();
                let closed_on_request = !capture.wanted() && loss == PLAYING;
                // Before anything else, so there is never a moment with two streams open on
                // one device.
                drop(stream);
                if watch.stopping() {
                    // Nothing to say: the app is going away.
                } else if closed_on_request {
                    info!("voice capture closed");
                    failures = 0;
                } else {
                    stopped("voice capture", loss, held, &mut failures, watch, pace);
                }
            }
            Err(err) => {
                // **Asked for, attempted, and not open — and *which* of those, because the
                // player is told.** Set here rather than on the first tick something is
                // wanted: the moment between `listen(true)` and a stream starting is not a
                // fault, and a flag set there would flash on every first press.
                //
                // The enumeration is what separates the two causes, and it is worth its cost
                // on this path alone: a named device the host does not list is one the player
                // can do something about, and everything else is not. Failures are paced by
                // `after_failure`, so this asks the host at most once every couple of seconds
                // while nothing is opening — never on a poll, and never while one is held.
                let seen = host.device_names();
                capture.set_fault(Some(match wanted.as_deref() {
                    Some(name) if !seen.iter().any(|found| found == name) => {
                        CaptureFault::NotAttached
                    }
                    _ => CaptureFault::WouldNotOpen,
                }));
                if failures.is_multiple_of(FAILURE_LOG_EVERY) {
                    warn!(
                        "no voice capture ({err}); nothing this player says is sent. \
                         Devices seen: {seen:?}"
                    );
                    // The one place the list is refreshed while nothing opens, so a
                    // microphone chosen and then unplugged still leaves the knob offering
                    // what is actually attached.
                    choice.publish(seen);
                }
                failures = failures.saturating_add(1);
                watch.rest(pace.after_failure);
            }
        }
    }
}

/// The capture supervisor thread, as the ECS holds it.
///
/// [`AudioDevice`]'s counterpart: a handle, a stop flag and the shared ring. Building one is
/// infallible for the same reason — a client that cannot start an audio thread is a client
/// nobody can hear, not one that will not start.
#[derive(Resource, Debug)]
pub struct AudioCapture {
    watch: Arc<Watch>,
    capture: Arc<Capture>,
    /// What the host named on this side of the card. The same type the output supervisor
    /// keeps, and only its list half is used yet: naming an input device is #853 part 5.
    choice: Arc<Choice>,
    supervisor: Option<JoinHandle<()>>,
}

impl AudioCapture {
    /// Starts the supervisor. **No device is opened until something calls
    /// [`Capture::listen`]** with `true`.
    pub fn start() -> Self {
        let watch = Arc::new(Watch::default());
        let capture = Arc::new(Capture::new());
        let choice = Arc::new(Choice::default());
        let supervisor = thread::Builder::new()
            .name("voxelheim-voice-capture".to_owned())
            .spawn({
                let watch = Arc::clone(&watch);
                let capture = Arc::clone(&capture);
                let choice = Arc::clone(&choice);
                move || supervise_capture(&CpalHost::new(), &capture, &watch, &choice, Pace::REAL)
            })
            .map_err(|err| {
                warn!(
                    "the voice capture thread would not start ({err}); nobody can hear this player"
                )
            })
            .ok();
        Self {
            watch,
            capture,
            choice,
            supervisor,
        }
    }

    /// Asks for the microphone called `name`, or for the system default when `None`.
    ///
    /// Recorded rather than acted on, exactly as [`AudioDevice::use_output`] is: the
    /// supervisor owns the stream and notices within one poll while it holds one, and reads
    /// this fresh on its next open when it does not.
    pub fn use_input(&self, name: Option<String>) {
        self.choice.want(name);
    }

    /// Every input device the host named, as of the supervisor's last enumeration.
    pub fn input_devices(&self) -> Vec<String> {
        self.choice.seen()
    }

    /// How many times that list has been replaced, so a Bevy system can tell "nothing new"
    /// from "the same list again" without a lock or a clone every frame.
    pub fn listings(&self) -> u64 {
        self.choice.listings()
    }

    /// The ring and the flags, for the pipeline above.
    // Read by the capture pipeline in #852 part 6, which is the first thing with a use for
    // a captured sample. See `audio/dsp.rs` for why the seam is here.
    #[allow(dead_code)]
    pub fn shared(&self) -> &Arc<Capture> {
        &self.capture
    }

    /// Asks for a stream to be open, or closed, and wakes the supervisor.
    ///
    /// **The wake is the point, and it is why this is here rather than on [`Capture`].** The
    /// flag lives with the ring, where the supervisor reads it; the condvar the supervisor
    /// sleeps on lives with [`Watch`], and only this resource holds both. Without the wake a
    /// press waits out a whole [`POLL_WHILE_PLAYING`] before a device is even asked for — half
    /// a second off the front of the first thing anybody says.
    // Called by the capture pipeline in #852 part 6.
    #[allow(dead_code)]
    pub fn listen(&self, wanted: bool) {
        self.capture.listen(wanted);
        self.watch.nudge();
    }
}

/// The seams `audio/voice.rs`'s tests drive the pipeline through, with no device anywhere.
///
/// Test-only for the reason [`AudioDevice::idle`] is: [`AudioCapture::start`] is the one
/// function that spawns the thread which would open one.
#[cfg(test)]
impl AudioCapture {
    /// An `AudioCapture` with no supervisor.
    pub(super) fn idle() -> Self {
        Self {
            watch: Arc::new(Watch::default()),
            capture: Arc::new(Capture::new()),
            choice: Arc::new(Choice::default()),
            supervisor: None,
        }
    }

    /// Publishes `names` as though the supervisor had just enumerated them.
    pub(super) fn enumerated(&self, names: Vec<String>) {
        self.choice.publish(names);
    }

    /// What [`Self::use_input`] last recorded.
    pub(super) fn wanted_input(&self) -> Option<String> {
        self.choice.wanted()
    }

    /// Records a stream as the supervisor would when one opens.
    pub(super) fn opened(&self, sample_rate: u32, channels: u16) {
        self.capture.opened_at(sample_rate, channels);
    }

    /// Records the supervisor as having tried to open a device and failed, for `fault`.
    pub(super) fn faulted(&self, fault: CaptureFault) {
        self.capture.set_fault(Some(fault));
    }

    /// Pushes one block as the capture callback would.
    pub(super) fn fed(&self, block: &[f32]) {
        self.capture.captured(block);
    }
}

impl Drop for AudioCapture {
    /// Dropping the resource is how the app says "close the microphone".
    fn drop(&mut self) {
        self.watch.stop();
        if let Some(supervisor) = self.supervisor.take() {
            let _ = supervisor.join();
        }
    }
}

impl InputHost for CpalHost {
    type Stream = cpal::Stream;

    fn device_names(&self) -> Vec<String> {
        match self.0.input_devices() {
            Ok(devices) => named(devices.map(|device| device.name())),
            // Not an error worth failing over: the list is a bound, and an empty one says
            // what a missing one would — this host offers nothing but its default.
            Err(_) => Vec::new(),
        }
    }

    fn open(
        &self,
        wanted: Option<&str>,
        capture: &Arc<Capture>,
        watch: &Arc<Watch>,
    ) -> Result<(cpal::Stream, Format), String> {
        let device = match wanted {
            Some(name) => self
                .named_input_device(name)
                .ok_or_else(|| format!("{name} is not one of this host's input devices"))?,
            None => self
                .0
                .default_input_device()
                .ok_or_else(|| "this host has no default input device".to_owned())?,
        };
        let name = device.name().ok();
        let config = float_input_config(&device).ok_or_else(|| {
            let shown = name.as_deref().unwrap_or(UNNAMED);
            format!("{shown} offers no 32-bit float input configuration")
        })?;
        let format = Format {
            name,
            sample_rate: config.sample_rate().0,
            channels: config.channels(),
        };

        let recording = Arc::clone(capture);
        let lost = Arc::clone(watch);
        let stream = device
            .build_input_stream(
                &config.config(),
                // The capture callback, and the whole of it.
                move |block: &[f32], _: &cpal::InputCallbackInfo| {
                    recording.captured(block);
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
            .map_err(|err| format!("cannot start the capture stream: {err}"))
    }
}

/// The float configuration this input device should be opened with, if it has one.
///
/// [`float_config`]'s reasoning, verbatim, on the input side: the device's *default* rate
/// rather than its maximum, because the platform's own mixer runs at the default and asking
/// for 192 kHz would only make it resample. `audio/dsp.rs` converts whatever this negotiates
/// to 48 kHz mono, so a rate closer to the device's own is a conversion this client does once
/// instead of one the operating system does first.
fn float_input_config(device: &cpal::Device) -> Option<SupportedStreamConfig> {
    let default = device.default_input_config().ok()?;
    if default.sample_format() == SampleFormat::F32 {
        return Some(default);
    }
    let wanted = default.sample_rate().0;
    device
        .supported_input_configs()
        .ok()?
        .filter(|range| range.sample_format() == SampleFormat::F32)
        .filter_map(|range| {
            let rate = wanted.clamp(range.min_sample_rate().0, range.max_sample_rate().0);
            range.try_with_sample_rate(SampleRate(rate))
        })
        .min_by_key(|config| (config.sample_rate().0.abs_diff(wanted), config.channels()))
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
        /// Mutable, so a test can plug a device in between two opens.
        names: Mutex<Vec<String>>,
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
        /// Which device each successive `open` was asked for.
        asked_for: Mutex<Vec<Option<String>>>,
    }

    impl FakeHost {
        /// A host answering `answers` in turn, its default named after the first Ok one.
        fn new(answers: Vec<Result<Format, String>>) -> Arc<Self> {
            Self::naming(&["a card"], answers)
        }

        /// The same, naming `names` when it is asked what it has.
        fn naming(names: &[&str], answers: Vec<Result<Format, String>>) -> Arc<Self> {
            let default = answers
                .iter()
                .flatten()
                .next()
                .and_then(|format| format.name.clone());
            Arc::new(Self {
                answers: Mutex::new(answers.into()),
                names: Mutex::new(names.iter().map(|name| (*name).to_owned()).collect()),
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
            lock(&self.names).clone()
        }

        fn default_name(&self) -> Option<String> {
            lock(&self.default).clone()
        }

        fn open(
            &self,
            wanted: Option<&str>,
            mixer: &Arc<Mixer>,
            watch: &Arc<Watch>,
        ) -> Result<(FakeStream, Format), String> {
            lock(&self.asked_for).push(wanted.map(str::to_owned));
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
        choice: Arc<Choice>,
        thread: Option<JoinHandle<()>>,
    }

    impl Running {
        fn start(host: &Arc<FakeHost>, pace: Pace) -> Self {
            let mixer = Arc::new(Mixer::new());
            let watch = Arc::new(Watch::default());
            let choice = Arc::new(Choice::default());
            let thread = thread::spawn({
                let host = Arc::clone(host);
                let mixer = Arc::clone(&mixer);
                let watch = Arc::clone(&watch);
                let choice = Arc::clone(&choice);
                move || supervise(&*host, &mixer, &watch, &choice, pace)
            });
            Self {
                mixer,
                watch,
                choice,
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

    /// **The knob's whole job, end to end.** The list the host names reaches the settings
    /// side; a name is asked for; the supervisor closes the stream it holds and opens that
    /// one instead, without being woken.
    #[test]
    fn choosing_a_device_reopens_the_stream_on_it() {
        let host = FakeHost::naming(
            &["Built-in speakers", "USB headset"],
            // Neither rate is `DEFAULT_SAMPLE_RATE`, so "the mixer reached this format" is
            // never satisfied by a mixer no stream has spoken to yet.
            vec![
                Ok(format("Built-in speakers", 44_100, 2)),
                Ok(format("USB headset", 32_000, 2)),
            ],
        );
        let mut running = Running::start(&host, BRISK);
        assert!(until(|| running.mixer.sample_rate() == 44_100));
        assert_eq!(
            running.choice.seen(),
            vec!["Built-in speakers".to_owned(), "USB headset".to_owned()],
            "the knob is offered what the host named"
        );
        assert_eq!(
            lock(&host.asked_for).first().cloned(),
            Some(None),
            "an untouched setting asks for no device in particular"
        );

        // Plugged in between the two opens, so the reopen is what has to notice it: the
        // list is refreshed on a stream that opened, not on a poll.
        lock(&host.names).push("Studio monitors".to_owned());
        running.choice.want(Some("USB headset".to_owned()));

        assert!(
            until(|| running.mixer.sample_rate() == 32_000),
            "the chosen device's format never reached the mixer"
        );
        assert!(
            until(|| running.choice.seen().len() == 3),
            "a device that appeared between two opens never reached the knob"
        );
        assert!(running.stop());
        assert_eq!(
            lock(&host.asked_for).last().cloned(),
            Some(Some("USB headset".to_owned())),
            "the reopen asked for the device the player picked"
        );
        assert!(
            lock(&host.found_live).iter().all(|live| *live == 0),
            "the old stream was still open when the new one was asked for"
        );
    }

    /// **A chosen device neither follows the system default nor is replaced when it is
    /// absent.** Both halves of "this device and no other" — the second is what makes the
    /// first worth having, so they are asserted together.
    #[test]
    fn a_chosen_device_neither_follows_the_default_nor_is_replaced() {
        let host = FakeHost::naming(
            &["Built-in speakers", "USB headset"],
            vec![Ok(format("USB headset", 44_100, 2))],
        );
        let mut running = Running::start(&host, BRISK);
        running.choice.want(Some("USB headset".to_owned()));

        // Wait for an open that *asked for the chosen name*, not merely for one that
        // produced 44,100 Hz. `start` spawns the supervisor before the line above runs, so
        // it may already have read `Choice::wanted()` as `None` and opened the default —
        // and this host answers every open with the same 44,100 Hz format, so a rate check
        // is satisfied by that pre-choice open while a `None -> Some` reopen is still
        // pending. Snapshotting `opens()` there would count the reopen against the sleep
        // below and fail a client that behaved correctly. Waiting on `asked_for` resolves
        // the transition first: it is immediately true when the choice beat the supervisor,
        // and becomes true at the reopen when it did not — after which nothing further is
        // pending, because the live `Choice` now matches what the current open captured.
        assert!(
            until(|| {
                lock(&host.asked_for).last().cloned() == Some(Some("USB headset".to_owned()))
            }),
            "the supervisor never opened the device the player chose"
        );
        assert_eq!(running.mixer.sample_rate(), 44_100);

        let opened = host.opens();
        *lock(&host.default) = Some("Built-in speakers".to_owned());
        // Several polls at `BRISK`, which is what would have reopened the stream had
        // `DEFAULT_MOVED` still applied to a device the player named.
        thread::sleep(Duration::from_millis(60));
        assert_eq!(host.opens(), opened, "the client left the chosen device");
        assert!(running.stop());

        // Absent, now: a name nothing answers to is a failure like any other — retried,
        // never a silent move to some other card.
        let absent = FakeHost::naming(
            &["Built-in speakers"],
            vec![Err(
                "USB headset is not one of this host's output devices".to_owned()
            )],
        );
        let mut running = Running::start(&absent, BRISK);
        running.choice.want(Some("USB headset".to_owned()));
        assert!(until(|| absent.opens() >= 3), "it keeps trying");
        assert_eq!(
            running.choice.seen(),
            vec!["Built-in speakers".to_owned()],
            "a machine where nothing opens still tells the knob what it has"
        );
        assert!(
            lock(&absent.asked_for)
                .iter()
                .all(|asked| asked.is_none() || asked.as_deref() == Some("USB headset")),
            "an attempt fell back to some other device"
        );
        assert_eq!(
            running.mixer.sample_rate(),
            DEFAULT_SAMPLE_RATE,
            "a device that never opened told the mixer nothing"
        );
        assert!(running.stop());
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

    // -------------------------------------------------------------------------
    // The microphone
    // -------------------------------------------------------------------------

    /// An [`InputHost`] that opens nothing and records what it was asked for.
    ///
    /// **No test below opens an input device either.** [`CpalHost`] is the only code here
    /// that reaches the platform, and nothing in this module constructs one.
    #[derive(Default)]
    struct FakeInput {
        answers: Mutex<VecDeque<Result<Format, String>>>,
        opens: AtomicUsize,
        live: Arc<AtomicUsize>,
        /// The capture the supervisor drives, kept so `open` can push samples into it the way
        /// a real callback would.
        captured: Mutex<Vec<f32>>,
        /// Makes the next `open` report a loss while `open` is still on the stack.
        lose_while_opening: AtomicBool,
        /// The same, for every `open` rather than the next one.
        always_lose_while_opening: AtomicBool,
        /// The capture's sample rate when each `start` was called — how a test sees whether
        /// the format was recorded before the callback could run.
        rate_at_start: Mutex<Vec<u32>>,
        /// Set by `open` so `start` can reach the shared state.
        shared: Mutex<Option<Arc<Capture>>>,
        /// What this host answers an enumeration with, and how many it has been asked for.
        names: Mutex<Vec<String>>,
        enumerations: AtomicUsize,
        /// The device asked for on each open, in order — `None` for the host's default.
        opened_names: Mutex<Vec<Option<String>>>,
    }

    impl FakeInput {
        fn answering(answers: Vec<Result<Format, String>>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.into()),
                ..Self::default()
            })
        }

        /// A host that answers `names` when asked what it has.
        fn naming(names: &[&str], answers: Vec<Result<Format, String>>) -> Arc<Self> {
            let host = Self::answering(answers);
            *lock(&host.names) = names.iter().map(|name| (*name).to_owned()).collect();
            host
        }

        fn opens(&self) -> usize {
            self.opens.load(Ordering::Relaxed)
        }

        fn enumerations(&self) -> usize {
            self.enumerations.load(Ordering::Relaxed)
        }

        /// What each open asked for, in order.
        fn opened_names(&self) -> Vec<Option<String>> {
            lock(&self.opened_names).clone()
        }
    }

    impl InputHost for Arc<FakeInput> {
        type Stream = FakeStream;

        fn device_names(&self) -> Vec<String> {
            self.enumerations.fetch_add(1, Ordering::Relaxed);
            lock(&self.names).clone()
        }

        fn open(
            &self,
            wanted: Option<&str>,
            capture: &Arc<Capture>,
            watch: &Arc<Watch>,
        ) -> Result<(FakeStream, Format), String> {
            self.opens.fetch_add(1, Ordering::Relaxed);
            lock(&self.opened_names).push(wanted.map(str::to_owned));
            if let Some(name) = wanted
                && !lock(&self.names).iter().any(|found| found == name)
            {
                // The real host's refusal, in twenty characters: a named device that is not
                // there is an error, and the supervisor above answers it by opening nothing
                // rather than by opening something else.
                return Err(format!("{name} is not one of this host's input devices"));
            }
            *lock(&self.shared) = Some(Arc::clone(capture));
            let answer = {
                let mut answers = lock(&self.answers);
                match answers.len() {
                    0 => Err("nothing left to answer".to_owned()),
                    1 => answers[0].clone(),
                    _ => answers.pop_front().expect("a queued answer"),
                }
            };
            if self.lose_while_opening.swap(false, Ordering::Relaxed)
                || self.always_lose_while_opening.load(Ordering::Relaxed)
            {
                watch.lose(DEVICE_GONE);
            }
            let format = answer?;
            self.live.fetch_add(1, Ordering::Relaxed);
            Ok((FakeStream(Arc::clone(&self.live)), format))
        }

        fn start(&self, _stream: &FakeStream) -> Result<(), String> {
            let capture = lock(&self.shared).clone();
            if let Some(capture) = capture {
                lock(&self.rate_at_start).push(
                    capture
                        .format()
                        .map(|(rate, _, _)| rate)
                        .unwrap_or_default(),
                );
                // What the platform's callback does, and the whole of it.
                let block = lock(&self.captured).clone();
                if !block.is_empty() {
                    capture.captured(&block);
                }
            }
            Ok(())
        }
    }

    /// Runs the capture supervisor until `done` answers true, then stops it and joins.
    fn drive_capture(
        host: &Arc<FakeInput>,
        capture: &Arc<Capture>,
        done: impl Fn() -> bool,
    ) -> Arc<Watch> {
        drive_capture_with(host, capture, &Arc::new(Choice::default()), done)
    }

    /// [`drive_capture`], with the enumeration the supervisor publishes into kept.
    fn drive_capture_with(
        host: &Arc<FakeInput>,
        capture: &Arc<Capture>,
        choice: &Arc<Choice>,
        done: impl Fn() -> bool,
    ) -> Arc<Watch> {
        let watch = Arc::new(Watch::default());
        let supervisor = {
            let host = Arc::clone(host);
            let capture = Arc::clone(capture);
            let watch = Arc::clone(&watch);
            let choice = Arc::clone(choice);
            thread::spawn(move || supervise_capture(&host, &capture, &watch, &choice, BRISK))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !done() && Instant::now() < deadline {
            thread::yield_now();
        }
        let reached = done();
        watch.stop();
        supervisor.join().expect("the supervisor ended cleanly");
        assert!(reached, "the supervisor never got there");
        watch
    }

    /// **The knob's bound is filled before anything is opened, and that is the point.**
    ///
    /// A player choosing a microphone has not asked to be recorded yet — the settings screen
    /// is up and the capture stream is shut — so a supervisor that enumerated only on an open
    /// would leave the knob with nothing to offer until after the choice had been made. And
    /// it asks **once** while idle, for [`supervise`]'s reason: enumeration is not free on
    /// every backend, and a list nobody is looking at must not be rebuilt on every poll.
    #[test]
    fn the_input_devices_are_named_before_a_microphone_is_ever_opened() {
        let host = FakeInput::naming(
            &["Built-in microphone", "USB headset mic"],
            vec![Ok(format("Built-in microphone", 48_000, 1))],
        );
        let capture = Arc::new(Capture::new());
        let choice = Arc::new(Choice::default());
        let listed = {
            let choice = Arc::clone(&choice);
            move || choice.listings() > 0
        };
        drive_capture_with(&host, &capture, &choice, listed);

        assert_eq!(
            choice.seen(),
            vec![
                "Built-in microphone".to_owned(),
                "USB headset mic".to_owned()
            ]
        );
        assert_eq!(
            host.opens(),
            0,
            "a device was opened to find out what the devices are called"
        );
        assert_eq!(
            host.enumerations(),
            1,
            "the idle loop asked the host what it had, over and over"
        );
    }

    /// **The named microphone is what gets opened, and an absent one is refused.**
    ///
    /// The two halves of the acceptance criterion as amended. Both sides of the card now
    /// answer the same way — a device that is chosen and not there is not opened and nothing
    /// is opened in its place — and `client/AGENTS.md` carries why, including the substituting
    /// version that was reversed. What refusing owes the player is being told, and
    /// [`Capture::unavailable`] is how the HUD finds out.
    #[test]
    fn a_named_microphone_is_opened_and_an_absent_one_is_refused() {
        let host = FakeInput::naming(
            &["Built-in microphone", "USB headset mic"],
            vec![Ok(format("USB headset mic", 48_000, 1))],
        );
        let capture = Arc::new(Capture::new());
        let choice = Arc::new(Choice::default());
        choice.want(Some("USB headset mic".to_owned()));
        capture.listen(true);

        let opened = {
            let host = Arc::clone(&host);
            move || host.opens() > 0
        };
        drive_capture_with(&host, &capture, &choice, opened);
        assert_eq!(
            host.opened_names(),
            vec![Some("USB headset mic".to_owned())],
            "the supervisor opened something other than the microphone the player named"
        );
        assert_eq!(
            capture.fault(),
            None,
            "a microphone that opened was reported at fault"
        );

        // And the same choice with that device gone: refused, over and over, and **never** the
        // other microphone the host does have. A player who named a headset must not be
        // recorded by whatever else is in the room.
        let host = FakeInput::naming(
            &["Built-in microphone"],
            vec![Ok(format("Built-in microphone", 48_000, 1))],
        );
        let capture = Arc::new(Capture::new());
        let choice = Arc::new(Choice::default());
        choice.want(Some("USB headset mic".to_owned()));
        capture.listen(true);

        let refused = {
            let capture = Arc::clone(&capture);
            move || capture.fault().is_some()
        };
        drive_capture_with(&host, &capture, &choice, refused);
        assert_eq!(
            capture.fault(),
            Some(CaptureFault::NotAttached),
            "a device the host does not list was reported as something else"
        );
        assert!(
            host.opened_names()
                .iter()
                .all(|asked| asked.as_deref() == Some("USB headset mic")),
            "the supervisor asked for a device the player never chose: {:?}",
            host.opened_names()
        );
        assert_eq!(
            capture.format(),
            None,
            "a stream was recorded for a device that was refused"
        );
        assert_eq!(
            choice.wanted(),
            Some("USB headset mic".to_owned()),
            "the refusal rewrote the player's choice"
        );
    }

    /// **A device that is there and will not open is a different sentence from one that is
    /// not there**, and this is the case the single flag could not tell apart.
    ///
    /// Busy, no float configuration, a stream that would not start, and a host with no default
    /// input at all all land here. The client cannot distinguish them from the outside and does
    /// not try — what it must not do is call any of them a missing device, because that sends a
    /// player to look for a cable while another application holds the microphone. Found by
    /// review on #928.
    #[test]
    fn a_device_that_is_present_and_will_not_open_is_not_reported_as_missing() {
        // Named, listed by the host, and refusing to open.
        let host = FakeInput::naming(
            &["USB headset mic"],
            vec![Err("the device is busy".to_owned())],
        );
        let capture = Arc::new(Capture::new());
        let choice = Arc::new(Choice::default());
        choice.want(Some("USB headset mic".to_owned()));
        capture.listen(true);

        let faulted = {
            let capture = Arc::clone(&capture);
            move || capture.fault().is_some()
        };
        drive_capture_with(&host, &capture, &choice, faulted);
        assert_eq!(
            capture.fault(),
            Some(CaptureFault::WouldNotOpen),
            "a device the host lists was reported as not connected"
        );

        // And the system default, which the player named nothing for: a host with no input at
        // all is not "the device you chose is missing" either, because they chose none.
        let host = FakeInput::naming(&[], vec![Err("no default input device".to_owned())]);
        let capture = Arc::new(Capture::new());
        let choice = Arc::new(Choice::default());
        capture.listen(true);

        let faulted = {
            let capture = Arc::clone(&capture);
            move || capture.fault().is_some()
        };
        drive_capture_with(&host, &capture, &choice, faulted);
        assert_eq!(
            capture.fault(),
            Some(CaptureFault::WouldNotOpen),
            "following the system default reported a device the player never named"
        );
    }

    /// **The flag is cleared by a device that turns up, and by nobody asking.**
    ///
    /// Which is what keeps "plug the headset back in and it just works" true with no state to
    /// go stale: the choice is re-read on every open attempt, so the first one after the
    /// device appears succeeds and the HUD stops saying anything.
    #[test]
    fn a_microphone_that_appears_clears_the_unavailable_flag() {
        let host = FakeInput::naming(&[], vec![Ok(format("USB headset mic", 48_000, 1))]);
        let capture = Arc::new(Capture::new());
        let choice = Arc::new(Choice::default());
        choice.want(Some("USB headset mic".to_owned()));
        capture.listen(true);

        let watch = Arc::new(Watch::default());
        let supervisor = {
            let host = Arc::clone(&host);
            let capture = Arc::clone(&capture);
            let watch = Arc::clone(&watch);
            let choice = Arc::clone(&choice);
            thread::spawn(move || supervise_capture(&host, &capture, &watch, &choice, BRISK))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while capture.fault().is_none() && Instant::now() < deadline {
            thread::yield_now();
        }
        let refused = capture.fault() == Some(CaptureFault::NotAttached);

        // The headset goes in.
        *lock(&host.names) = vec!["USB headset mic".to_owned()];
        while capture.fault().is_some() && Instant::now() < deadline {
            thread::yield_now();
        }
        let recovered = capture.fault().is_none();

        // And nothing wanted is not something missing.
        capture.listen(false);
        watch.nudge();
        *lock(&host.names) = Vec::new();
        let settled = Instant::now() + Duration::from_secs(2);
        while capture.fault().is_some() && Instant::now() < settled {
            thread::yield_now();
        }
        let idle_is_clear = capture.fault().is_none();

        watch.stop();
        supervisor.join().expect("the supervisor ended cleanly");

        assert!(
            refused,
            "an absent microphone was never reported as not attached"
        );
        assert!(
            recovered,
            "the microphone came back and the flag did not clear"
        );
        assert!(
            idle_is_clear,
            "a microphone nobody asked for was reported missing"
        );
    }

    /// **A microphone chosen while one is open is picked up**, which is `CHOICE_CHANGED` on
    /// this side of the card. There is no `DEFAULT_MOVED` twin, deliberately: a microphone is
    /// open only while somebody is speaking, and moving devices mid-sentence because the host
    /// renamed its default is not a thing this client does.
    #[test]
    fn choosing_a_different_microphone_reopens_on_it() {
        let host = FakeInput::naming(
            &["Built-in microphone", "USB headset mic"],
            vec![
                Ok(format("Built-in microphone", 48_000, 1)),
                Ok(format("USB headset mic", 48_000, 1)),
            ],
        );
        let capture = Arc::new(Capture::new());
        let choice = Arc::new(Choice::default());
        choice.want(Some("Built-in microphone".to_owned()));
        capture.listen(true);

        let watch = Arc::new(Watch::default());
        let supervisor = {
            let host = Arc::clone(&host);
            let capture = Arc::clone(&capture);
            let watch = Arc::clone(&watch);
            let choice = Arc::clone(&choice);
            thread::spawn(move || supervise_capture(&host, &capture, &watch, &choice, BRISK))
        };

        let deadline = Instant::now() + Duration::from_secs(5);
        while host.opens() < 1 && Instant::now() < deadline {
            thread::yield_now();
        }
        choice.want(Some("USB headset mic".to_owned()));
        while host.opens() < 2 && Instant::now() < deadline {
            thread::yield_now();
        }
        let reopened = host.opens() >= 2;
        watch.stop();
        supervisor.join().expect("the supervisor ended cleanly");

        assert!(reopened, "choosing another microphone never reopened");
        assert_eq!(
            host.opened_names(),
            vec![
                Some("Built-in microphone".to_owned()),
                Some("USB headset mic".to_owned())
            ]
        );
    }

    /// **The state a client that has never asked for voice sits in for ever.** The supervisor
    /// runs and no device is opened — which is the acceptance criterion's "the capture stream
    /// is never opened", and it is a property of this loop rather than of its callers.
    #[test]
    fn a_microphone_nobody_asked_for_is_never_opened() {
        let host = FakeInput::answering(vec![Ok(format("a microphone", 48_000, 1))]);
        let capture = Arc::new(Capture::new());
        assert!(!capture.wanted());

        let watch = Arc::new(Watch::default());
        let choice = Arc::new(Choice::default());
        let supervisor = {
            let host = Arc::clone(&host);
            let capture = Arc::clone(&capture);
            let watch = Arc::clone(&watch);
            let choice = Arc::clone(&choice);
            thread::spawn(move || supervise_capture(&host, &capture, &watch, &choice, BRISK))
        };
        // Long enough for hundreds of passes at `BRISK`.
        thread::sleep(Duration::from_millis(50));
        watch.stop();
        supervisor.join().expect("the supervisor ended cleanly");

        assert_eq!(host.opens(), 0, "a device was opened for nobody");
        assert_eq!(
            capture.format(),
            None,
            "a format was recorded for no stream"
        );
    }

    /// Asked for, opened; unasked, closed. And the format is recorded **before** the stream
    /// starts, so no callback can run while a consumer would read the previous stream's rate.
    #[test]
    fn a_stream_opens_when_it_is_asked_for_and_closes_when_it_is_not() {
        let host = FakeInput::answering(vec![Ok(format("a microphone", 44_100, 2))]);
        let capture = Arc::new(Capture::new());
        capture.listen(true);

        let live = Arc::clone(&host.live);
        // **Waited on the thing that is asserted, not on the open count.** `opens` is bumped
        // at the *top* of `open`, before the format is recorded, so a test that waited on it
        // could read the format a microsecond too early — which it did, in about one run in
        // three. `rate_at_start` is pushed by `start`, which is the last step of a completed
        // open, so it is the point after which every assertion below is settled.
        let opened = {
            let host = Arc::clone(&host);
            move || !lock(&host.rate_at_start).is_empty()
        };
        let watch = Arc::new(Watch::default());
        let choice = Arc::new(Choice::default());
        let supervisor = {
            let host = Arc::clone(&host);
            let capture = Arc::clone(&capture);
            let watch = Arc::clone(&watch);
            let choice = Arc::clone(&choice);
            thread::spawn(move || supervise_capture(&host, &capture, &watch, &choice, BRISK))
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !opened() && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(host.opens(), 1);
        assert_eq!(capture.format(), Some((44_100, 2, 1)));
        assert_eq!(
            lock(&host.rate_at_start).as_slice(),
            &[44_100],
            "the stream started before its format was recorded"
        );

        // Now say no, and the stream is dropped without the supervisor ending.
        capture.listen(false);
        let deadline = Instant::now() + Duration::from_secs(5);
        while live.load(Ordering::Relaxed) != 0 && Instant::now() < deadline {
            thread::yield_now();
        }
        assert_eq!(
            live.load(Ordering::Relaxed),
            0,
            "the microphone stayed open after it was let go"
        );

        watch.stop();
        supervisor.join().expect("the supervisor ended cleanly");
        assert_eq!(host.opens(), 1, "it reopened a device nobody asked for");
    }

    /// A microphone that will not open is a log line and a client nobody can hear, never a
    /// panic and never a spin — [`supervise`]'s rule, on the other direction.
    #[test]
    fn a_microphone_that_will_not_open_is_retried_rather_than_fatal() {
        let host = FakeInput::answering(vec![
            Err("no default input device".to_owned()),
            Err("still nothing".to_owned()),
            Ok(format("a microphone", 48_000, 1)),
        ]);
        let capture = Arc::new(Capture::new());
        capture.listen(true);
        drive_capture(&host, &capture, {
            let capture = Arc::clone(&capture);
            // The format, not the open count: see the note in the test above.
            move || capture.format().is_some()
        });
        assert_eq!(
            capture.format(),
            Some((48_000, 1, 1)),
            "the third attempt's format was never recorded"
        );
    }

    /// **A microphone that will not open is a wait, not a spin** — and the module doc says so,
    /// which until #919 it had no right to on either supervisor. Mutation-checked: deleting
    /// `watch.rest(pace.after_failure)` from the `Err` arm left the whole suite green before
    /// this test existed.
    #[test]
    fn a_failing_capture_open_waits_rather_than_spinning() {
        let host = FakeInput::answering(vec![Err("no default input device".to_owned())]);
        let capture = Arc::new(Capture::new());
        capture.listen(true);
        let watch = Arc::new(Watch::default());
        let supervisor = {
            let host = Arc::clone(&host);
            let capture = Arc::clone(&capture);
            let watch = Arc::clone(&watch);
            thread::spawn(move || {
                supervise_capture(
                    &host,
                    &capture,
                    &watch,
                    &Arc::new(Choice::default()),
                    Pace {
                        playing: Duration::from_millis(1),
                        after_failure: Duration::from_secs(2),
                    },
                )
            })
        };

        let deadline = Instant::now() + Duration::from_secs(5);
        while host.opens() == 0 && Instant::now() < deadline {
            thread::yield_now();
        }
        thread::sleep(Duration::from_millis(60));
        assert_eq!(
            host.opens(),
            1,
            "one attempt, then a wait — a retry loop that spun would have made hundreds"
        );

        // And the wait is interruptible: stopping does not have to outlast it.
        let stopped = Instant::now();
        watch.stop();
        supervisor.join().expect("the supervisor ended cleanly");
        assert!(stopped.elapsed() < Duration::from_secs(2));
    }

    /// **A stream that dies before it ever runs is paced and throttled like any other
    /// failure.** It is the one path where the hold loop's body never executes, so nothing
    /// waits unless something says so — measured at 588 591 reopens in 200 ms before #919.
    #[test]
    fn a_capture_stream_that_never_runs_is_not_reopened_in_a_spin() {
        let host = FakeInput::answering(vec![Ok(format("a microphone", 48_000, 1))]);
        // Every attempt loses the stream while `open` is still on the stack.
        host.always_lose_while_opening
            .store(true, Ordering::Relaxed);
        let capture = Arc::new(Capture::new());
        capture.listen(true);
        let watch = Arc::new(Watch::default());
        let supervisor = {
            let host = Arc::clone(&host);
            let capture = Arc::clone(&capture);
            let watch = Arc::clone(&watch);
            thread::spawn(move || {
                supervise_capture(
                    &host,
                    &capture,
                    &watch,
                    &Arc::new(Choice::default()),
                    Pace {
                        playing: Duration::from_millis(1),
                        after_failure: Duration::from_secs(2),
                    },
                )
            })
        };

        let deadline = Instant::now() + Duration::from_secs(5);
        while host.opens() == 0 && Instant::now() < deadline {
            thread::yield_now();
        }
        thread::sleep(Duration::from_millis(60));
        let opens = host.opens();
        assert!(
            opens <= 2,
            "a stream that never ran was reopened {opens} times in 60 ms"
        );

        watch.stop();
        supervisor.join().expect("the supervisor ended cleanly");
    }

    /// **The first press must not wait out a poll.** `AudioCapture::listen` wakes the
    /// supervisor, so a device is asked for at once rather than up to `POLL_WHILE_PLAYING`
    /// later — half a second off the front of the first thing anybody says.
    #[test]
    fn asking_for_a_microphone_wakes_the_supervisor_rather_than_waiting_out_a_poll() {
        let host = FakeInput::answering(vec![Ok(format("a microphone", 48_000, 1))]);
        let capture = Arc::new(Capture::new());
        let watch = Arc::new(Watch::default());
        // A pace whose idle poll is far longer than this test will wait for.
        let pace = Pace {
            playing: Duration::from_secs(30),
            after_failure: Duration::from_secs(30),
        };
        let choice = Arc::new(Choice::default());
        let supervisor = {
            let host = Arc::clone(&host);
            let capture = Arc::clone(&capture);
            let watch = Arc::clone(&watch);
            let choice = Arc::clone(&choice);
            thread::spawn(move || supervise_capture(&host, &capture, &watch, &choice, pace))
        };

        // Let it reach the idle wait before anything asks.
        thread::sleep(Duration::from_millis(30));
        assert_eq!(host.opens(), 0, "a device was opened for nobody");

        let asked = Instant::now();
        capture.listen(true);
        watch.nudge();
        let deadline = asked + Duration::from_secs(5);
        while host.opens() == 0 && Instant::now() < deadline {
            thread::yield_now();
        }
        let waited = asked.elapsed();
        assert_eq!(host.opens(), 1, "the press opened nothing");
        assert!(
            waited < Duration::from_secs(1),
            "the press waited {waited:?}, which is the poll rather than a wake"
        );

        watch.stop();
        supervisor.join().expect("the supervisor ended cleanly");
    }

    /// A stream that dies while it is starting reports its loss with `open` still on the
    /// stack. Clearing the loss code afterwards would discard the only notification there is,
    /// and this is the mirror of the output side's assertion.
    #[test]
    fn a_capture_stream_that_dies_while_opening_is_reopened() {
        let host = FakeInput::answering(vec![Ok(format("a microphone", 48_000, 1))]);
        host.lose_while_opening.store(true, Ordering::Relaxed);
        let capture = Arc::new(Capture::new());
        capture.listen(true);
        drive_capture(&host, &capture, {
            let host = Arc::clone(&host);
            move || host.opens() >= 2
        });
        assert!(
            capture
                .format()
                .is_some_and(|(_, _, generation)| generation >= 2),
            "the second stream was never recorded as a new one"
        );
    }

    /// **The ring, in the order it was written, and what would not fit.**
    ///
    /// The ramp is the point: with one value in every slot the ordering assertion below holds
    /// for any subset in any order, which is what the review on #919 found — inverting
    /// `captured` to keep the newest left the whole suite green. Part 5 counts the overruns
    /// and says only that, because nothing there can read the ring at all; this is the part
    /// where `Capture::take` exists, so this is where the ordering can be seen.
    #[test]
    fn the_capture_ring_is_read_in_order_and_counts_what_it_could_not_hold() {
        let capture = Capture::new();
        capture.opened_at(48_000, 1);
        let mut heard = Vec::new();
        // The first read of a stream is the skip: see `Capture::take`.
        assert_eq!(
            capture.take(&mut heard),
            Some(Captured {
                sample_rate: 48_000,
                channels: 1,
                fresh: true
            })
        );

        capture.captured(&[0.25, -0.5, 0.75]);
        assert_eq!(capture.take(&mut heard).map(|read| read.fresh), Some(false));
        assert_eq!(heard, vec![0.25, -0.5, 0.75]);
        assert_eq!(capture.take(&mut heard).map(|read| read.fresh), Some(false));
        assert_eq!(heard.len(), 3, "reading nothing changed the buffer");
        assert_eq!(capture.overruns(), 0);

        // **A ramp, not a constant, and the review on #919 is why.** With the same value in
        // every slot the assertion below holds for any subset in any order — mutating
        // `captured` to keep the *newest* and drop the oldest left the whole suite green. A
        // ramp makes the identity of the first and last sample the thing under test.
        let ramp: Vec<f32> = (0..CAPTURE_CAPACITY + 40).map(|at| at as f32).collect();
        capture.captured(&ramp);
        assert_eq!(capture.overruns(), 40);
        heard.clear();
        capture.take(&mut heard).expect("a stream is open");
        assert_eq!(heard.len(), CAPTURE_CAPACITY);
        assert_eq!(heard[0], 0.0, "the oldest sample was dropped or reordered");
        assert_eq!(
            heard[CAPTURE_CAPACITY - 1],
            (CAPTURE_CAPACITY - 1) as f32,
            "the ring kept the newest samples rather than refusing them"
        );
        assert!(
            heard
                .iter()
                .enumerate()
                .all(|(at, sample)| *sample == at as f32),
            "the ring did not read back in the order it was written"
        );
    }

    /// Two streams can negotiate the same rate and still be two streams, and the three fields
    /// are read as one word so a consumer can never see one stream's rate beside another's
    /// identity.
    #[test]
    fn a_second_stream_at_the_same_rate_is_still_a_new_stream() {
        let capture = Capture::new();
        assert_eq!(capture.format(), None, "a format before any stream");
        capture.opened_at(48_000, 1);
        assert_eq!(capture.format(), Some((48_000, 1, 1)));
        capture.opened_at(48_000, 1);
        assert_eq!(
            capture.format(),
            Some((48_000, 1, 2)),
            "the samples across a reopen are a gap, not a continuation"
        );

        // The packing is what makes the tuple atomic, so it is worth pinning that the three
        // fields survive it — including a generation that has wrapped past its ceiling.
        assert_eq!(
            Capture::pack(44_100, 2, 7),
            u64::from(44_100u32) | (2u64 << 32) | (7u64 << 48)
        );
        let wrapped = Capture::new();
        wrapped
            .stream
            .store(Capture::pack(1, 1, u16::MAX), Ordering::Relaxed);
        wrapped.opened_at(96_000, 2);
        assert_eq!(
            wrapped.format(),
            Some((96_000, 2, 1)),
            "the generation wrapped onto zero, which means no stream at all"
        );
    }

    /// **The seam a reopen leaves, and the whole reason `take` is a method.**
    ///
    /// The ring can hold the tail of one stream and the head of the next at two different
    /// sample rates, with nothing marking where one ends. A naive drain hands both back as
    /// one batch — the review on #919 found exactly that — so the first read of a stream this
    /// reader has not seen throws the ring away and says so.
    #[test]
    fn samples_are_never_spliced_across_a_reopen() {
        let capture = Capture::new();
        capture.opened_at(48_000, 1);
        let mut heard = Vec::new();
        capture
            .take(&mut heard)
            .expect("the first read is the skip");

        // Half a second of the first stream, read normally.
        capture.captured(&[0.5; 100]);
        assert_eq!(capture.take(&mut heard).map(|read| read.fresh), Some(false));
        assert_eq!(heard.len(), 100);

        // Its tail is still in the ring when the device is reopened at another rate, and more
        // arrives from the new stream before anything reads again.
        capture.captured(&[0.5; 40]);
        capture.opened_at(16_000, 2);
        capture.captured(&[-0.25; 60]);

        let read = capture.take(&mut heard).expect("a stream is open");
        assert!(read.fresh, "a reopen was reported as a continuation");
        assert_eq!(read.sample_rate, 16_000, "the old stream's rate came back");
        assert_eq!(
            heard.len(),
            100,
            "the tail of one stream was spliced onto the head of another"
        );

        // And from there the new stream reads normally.
        capture.captured(&[-0.75; 10]);
        let read = capture.take(&mut heard).expect("a stream is open");
        assert!(!read.fresh);
        assert_eq!(heard.len(), 110);
        assert!(heard[100..].iter().all(|sample| *sample == -0.75));
    }

    /// A stream that opens *while* a read is running makes the batch two streams spliced,
    /// and there is no way to say where the seam is — so the batch is discarded and the
    /// caller's buffer is put back exactly as it was.
    ///
    /// The reopen is placed by [`Capture::reopen_mid_read`]; that field's doc says why a
    /// fixture has to place it rather than race for it.
    #[test]
    fn a_reopen_during_a_read_discards_the_batch_rather_than_handing_it_over() {
        let capture = Capture::new();
        capture.opened_at(48_000, 1);
        let mut heard = vec![9.0, 9.0];
        capture
            .take(&mut heard)
            .expect("the first read is the skip");

        capture.captured(&[0.5; 20]);
        assert_eq!(capture.take(&mut heard).map(|read| read.fresh), Some(false));
        assert_eq!(heard.len(), 22, "an ordinary read appends");

        // Now the reopen lands between the two generation reads inside `take`.
        capture.captured(&[0.5; 30]);
        capture.reopen_mid_read.store(true, Ordering::Relaxed);
        assert_eq!(
            capture.take(&mut heard),
            None,
            "a batch spanning a reopen was handed over"
        );
        assert_eq!(heard.len(), 22, "the caller's buffer was left changed");

        // And the next read reports a fresh stream, so the caller throws its own carried
        // state away.
        assert!(
            capture.take(&mut heard).is_some_and(|read| read.fresh),
            "the read after a discarded batch did not report a fresh stream"
        );
        assert_eq!(heard.len(), 22, "a fresh stream appended something");

        // **And the same on the skipping path**, which is the half the review on #921 found
        // unprotected: a reopen that lands while a *fresh* read is skipping would otherwise
        // answer with the ended stream's rate and leave the reader's position on the old
        // generation, so the call after it would report `fresh` a second time.
        capture.opened_at(16_000, 2);
        capture.captured(&[0.5; 30]);
        capture.reopen_mid_read.store(true, Ordering::Relaxed);
        assert_eq!(
            capture.take(&mut heard),
            None,
            "a skip that a reopen ran through answered anyway"
        );
        let after = capture.take(&mut heard).expect("a stream is open");
        assert!(after.fresh);
        assert_eq!(
            after.sample_rate, 16_000,
            "the answer described the stream that ended"
        );
        assert!(
            !capture.take(&mut heard).expect("a stream is open").fresh,
            "the same stream was reported fresh twice"
        );
    }
}
