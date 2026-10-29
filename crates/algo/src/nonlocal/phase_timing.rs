//! Per-phase wall-clock timers and counters for [`super::complete_with`], for the A/B benchmark
//! (`examples/bench_nonlocal.rs`). Off by default: without the `phase-timing` cargo feature every
//! call here is an empty inline function, so call sites need no `cfg`. With it, relaxed atomics
//! accumulate the time spent in each phase and the number of `exp` evaluations of the similarity
//! weights; [`reset`] and [`snapshot`] read them. On wasm32 only the counters run (no clock).
//!
//! Timers measure the calling thread's wall-clock time in a phase, so they are meaningful for
//! one completion at a time (the bench runs them sequentially).

/// A phase of the completion that is timed on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Onion-peel initialisation of the coarsest level.
    OnionPeel,
    /// PatchMatch passes.
    PatchMatch,
    /// Refreshing the target costs.
    RefreshCosts,
    /// The weighted vote (including the uniform one after upsampling).
    Vote,
    /// The final stage: `copy_best`, `blend_seams` and `fuse_seams`.
    Final,
}

#[cfg(feature = "phase-timing")]
pub use on::{Snapshot, reset, snapshot};

#[cfg(feature = "phase-timing")]
mod on {
    use super::Phase;
    use std::sync::atomic::{AtomicU64, Ordering};

    const PHASES: usize = 5;
    #[allow(clippy::declare_interior_mutable_const)]
    const ZERO: AtomicU64 = AtomicU64::new(0);
    static NANOS: [AtomicU64; PHASES] = [ZERO; PHASES];
    static CALLS: [AtomicU64; PHASES] = [ZERO; PHASES];
    static EXP_VOTE: AtomicU64 = AtomicU64::new(0);
    static EXP_ONION: AtomicU64 = AtomicU64::new(0);

    /// Accumulated phase times and counters since the last [`reset`].
    #[derive(Clone, Copy, Debug, Default, PartialEq)]
    pub struct Snapshot {
        /// Nanoseconds per phase, in the order of [`Snapshot::NAMES`].
        pub nanos: [u64; PHASES],
        /// How many times each phase ran.
        pub calls: [u64; PHASES],
        /// `exp` evaluations of similarity weights in the vote.
        pub exp_vote: u64,
        /// `exp` evaluations of similarity weights in the onion peel.
        pub exp_onion: u64,
    }

    impl Snapshot {
        /// Phase names, matching the index of [`Snapshot::nanos`] and [`Snapshot::calls`].
        pub const NAMES: [&'static str; PHASES] = ["onion_peel", "patchmatch", "refresh_costs", "vote", "final"];
    }

    /// Zero every timer and counter.
    pub fn reset() {
        for a in NANOS.iter().chain(&CALLS).chain([&EXP_VOTE, &EXP_ONION]) {
            a.store(0, Ordering::Relaxed);
        }
    }

    /// The current values.
    pub fn snapshot() -> Snapshot {
        let load = |a: &[AtomicU64; PHASES]| {
            let mut out = [0u64; PHASES];
            for (o, a) in out.iter_mut().zip(a) {
                *o = a.load(Ordering::Relaxed);
            }
            out
        };
        Snapshot { nanos: load(&NANOS), calls: load(&CALLS), exp_vote: EXP_VOTE.load(Ordering::Relaxed), exp_onion: EXP_ONION.load(Ordering::Relaxed) }
    }

    /// Adds the time from its creation to its drop to a phase.
    pub(super) struct Timer {
        phase: Phase,
        #[cfg(not(target_arch = "wasm32"))]
        start: std::time::Instant,
    }

    impl Timer {
        #[inline]
        pub(super) fn start(phase: Phase) -> Self {
            Self {
                phase,
                #[cfg(not(target_arch = "wasm32"))]
                start: std::time::Instant::now(),
            }
        }
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            let i = self.phase as usize;
            #[cfg(not(target_arch = "wasm32"))]
            {
                let ns = u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX);
                if let Some(a) = NANOS.get(i) {
                    a.fetch_add(ns, Ordering::Relaxed);
                }
            }
            if let Some(a) = CALLS.get(i) {
                a.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    #[inline]
    pub(super) fn add_exp_vote(n: u64) {
        EXP_VOTE.fetch_add(n, Ordering::Relaxed);
    }

    #[inline]
    pub(super) fn add_exp_onion(n: u64) {
        EXP_ONION.fetch_add(n, Ordering::Relaxed);
    }
}

#[cfg(not(feature = "phase-timing"))]
mod off {
    use super::Phase;

    /// Does nothing (the `phase-timing` feature is off).
    pub(super) struct Timer;

    impl Timer {
        #[inline(always)]
        pub(super) fn start(_phase: Phase) -> Self {
            Self
        }
    }

    #[inline(always)]
    pub(super) fn add_exp_vote(_n: u64) {}

    #[inline(always)]
    pub(super) fn add_exp_onion(_n: u64) {}
}

#[cfg(not(feature = "phase-timing"))]
use off::{Timer, add_exp_onion, add_exp_vote};
#[cfg(feature = "phase-timing")]
use on::{Timer, add_exp_onion, add_exp_vote};

/// Times a phase until the returned guard drops.
#[inline(always)]
pub(super) fn timer(phase: Phase) -> impl Sized {
    Timer::start(phase)
}

/// Counts `n` similarity-weight `exp` evaluations in the vote.
#[inline(always)]
pub(super) fn count_vote_exps(n: u64) {
    add_exp_vote(n);
}

/// Counts `n` similarity-weight `exp` evaluations in the onion peel.
#[inline(always)]
pub(super) fn count_onion_exps(n: u64) {
    add_exp_onion(n);
}
