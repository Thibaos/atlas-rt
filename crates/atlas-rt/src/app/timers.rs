use std::{
    collections::HashMap,
    time::{Duration, Instant},
};

pub enum Condition {
    EveryFrame,
    Every(Duration),
}

pub struct ScheduleController {
    schedules: HashMap<&'static str, (Instant, Condition)>,
}

impl ScheduleController {
    pub fn new() -> Self {
        Self {
            schedules: HashMap::new(),
        }
    }

    pub fn add_schedule_duration(&mut self, name: &'static str, duration: Duration) {
        let instant = Instant::now();
        self.schedules
            .insert(name, (instant, Condition::Every(duration)));
    }

    pub fn add_schedule_frames(&mut self, name: &'static str) {
        let instant = Instant::now();
        self.schedules
            .insert(name, (instant, Condition::EveryFrame));
    }

    pub fn check(&mut self, key: &str) -> Option<Duration> {
        if let Some((last_update_instant, condition)) = self.schedules.get_mut(key) {
            match condition {
                Condition::EveryFrame => {
                    let duration = last_update_instant.elapsed();
                    *last_update_instant = Instant::now();

                    Some(duration)
                }
                Condition::Every(duration) => {
                    if (*last_update_instant).elapsed() > *duration {
                        *last_update_instant = Instant::now();

                        Some(*duration)
                    } else {
                        None
                    }
                }
            }
        } else {
            None
        }
    }
}
