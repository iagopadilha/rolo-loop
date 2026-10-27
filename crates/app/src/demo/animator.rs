// SPDX-License-Identifier: MIT
//! Thread that advances the demo story every few seconds (scaled by --speed) while the app serves the panel.
//!
//! It holds `LOOP.lock` with its own PID while a turn runs (the panel shows `running`), waits on the loop's question for a while (an answer clicked in
//! the panel, recorded by the engine stub, ends the wait early), and with `loop_forever` restarts the story after it ends. Never spawns `claude`.

use super::catalog::{Action, TurnPlan, SEEDED_TURNS};
use super::player::{AnswerProvider, DemoStoryPlayer};
use super::seeder::DemoProjectSeeder;
use engine_state::loop_state;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

pub const STEP_SECONDS: f64 = 3.0;
pub const DONE_HOLD_SECONDS: f64 = 12.0;
const ANSWER_POLL_SECONDS: f64 = 0.25;
pub const ANSWER_FILE: &str = "demo-answer.txt";

/// Seconds one story cycle takes from the first animated step to `done` (steps x STEP_SECONDS + the question hold).
pub fn compute_story_seconds(turn_plans: &[TurnPlan], speed: f64) -> f64 {
    let animated = turn_plans.iter().filter(|(number, _)| *number > SEEDED_TURNS);
    let steps: usize = animated.clone().map(|(_, steps)| steps.len()).sum();
    let holds: f64 = animated
        .flat_map(|(_, steps)| steps.iter().flatten())
        .map(|action| if let Action::WaitForAnswer { hold_seconds, .. } = action { *hold_seconds } else { 0.0 })
        .sum();
    (steps as f64 * STEP_SECONDS + holds) / speed
}

/// A stop flag that sleeping code can wait on.
#[derive(Default)]
pub struct StopSignal {
    stopped: AtomicBool,
    gate: Mutex<()>,
    wake: Condvar,
}

impl StopSignal {
    pub fn set(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _guard = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        self.wake.notify_all();
    }

    pub fn is_set(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Sleeps up to `seconds`, waking early when stopped.
    pub fn wait(&self, seconds: f64) {
        let guard = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        if self.is_set() {
            return;
        }
        let _ = self.wake.wait_timeout(guard, Duration::from_secs_f64(seconds.max(0.0)));
    }
}

fn answer_file(repo_dir: &str) -> PathBuf {
    loop_state::loop_path(repo_dir, ANSWER_FILE)
}

fn read_clicked_answer(repo_dir: &str) -> Option<String> {
    let path = answer_file(repo_dir);
    if !path.is_file() {
        return None;
    }
    let text = loop_state::read(&path).trim().to_string();
    let _ = std::fs::remove_file(&path);
    (!text.is_empty()).then_some(text)
}

/// Holds the question for `hold_seconds` (scaled); an answer clicked in the panel ends the wait and wins.
fn build_answer_provider(repo_dir: String, speed: f64, stop: Arc<StopSignal>) -> AnswerProvider {
    Arc::new(move |_question: &str, hold_seconds: f64, answer: &str| {
        let deadline = Instant::now() + Duration::from_secs_f64(hold_seconds / speed);
        while Instant::now() < deadline && !stop.is_set() {
            if let Some(clicked) = read_clicked_answer(&repo_dir) {
                return clicked;
            }
            stop.wait(ANSWER_POLL_SECONDS);
        }
        read_clicked_answer(&repo_dir).unwrap_or_else(|| answer.to_string())
    })
}

pub struct DemoAnimator {
    pub stop: Arc<StopSignal>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl DemoAnimator {
    pub fn start(mut seeder: DemoProjectSeeder, mut player: DemoStoryPlayer, log: Arc<dyn Fn(&str, &str) + Send + Sync>, speed: f64, loop_forever: bool) -> Self {
        let speed = if speed.is_finite() { speed.max(0.05) } else { 1.0 };
        let stop = Arc::new(StopSignal::default());
        let provider = build_answer_provider(seeder.repo_dir.clone(), speed, stop.clone());
        player.keeps_lock = true;
        player.answer_provider = provider.clone();
        let thread_stop = stop.clone();
        let handle = std::thread::Builder::new()
            .name("demo-animator".into())
            .spawn(move || {
                let stop = thread_stop;
                while !stop.is_set() {
                    if !player.is_finished() {
                        player.apply_next_step();
                        stop.wait(STEP_SECONDS / speed);
                        continue;
                    }
                    log("demo_story_done", &format!("turns={}", player.get_completed_turns()));
                    if !loop_forever {
                        break;
                    }
                    stop.wait(DONE_HOLD_SECONDS / speed);
                    if stop.is_set() {
                        break;
                    }
                    match seeder.reset_story(true, Some(provider.clone())) {
                        Ok(fresh) => {
                            player = fresh;
                            log("demo_story_restarted", &format!("generation={}", seeder.generation));
                        }
                        Err(failure) => {
                            log("demo_animator_failed", &failure.chars().take(300).collect::<String>());
                            break;
                        }
                    }
                }
                player.release_lock();
            })
            .ok();
        DemoAnimator { stop, handle }
    }

    pub fn stop_and_join(&mut self) {
        self.stop.set();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo::catalog::build_turn_plans;

    #[test]
    fn the_cycle_counts_steps_after_the_seeded_turns_plus_the_hold() {
        let plans = build_turn_plans();
        let normal = compute_story_seconds(&plans, 1.0);
        assert!(normal > 0.0);
        assert!((compute_story_seconds(&plans, 2.0) - normal / 2.0).abs() < 1e-9);
    }

    #[test]
    fn a_stop_wakes_a_sleeper() {
        let signal = Arc::new(StopSignal::default());
        let waiter = signal.clone();
        let started = Instant::now();
        let thread = std::thread::spawn(move || waiter.wait(30.0));
        std::thread::sleep(Duration::from_millis(50));
        signal.set();
        thread.join().unwrap();
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
