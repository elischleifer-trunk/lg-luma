//! Animation time; `--frames` runs step it per loop iteration (see development.md).

use std::{
    cell::Cell,
    time::{Duration, Instant},
};

pub const FIXED_STEP: Duration = Duration::from_micros(16_667);

thread_local! {
    static FIXED: Cell<Option<Instant>> = const { Cell::new(None) };
}

pub fn now() -> Instant {
    FIXED.with(Cell::get).unwrap_or_else(Instant::now)
}

pub fn since(start: Instant) -> Duration {
    now().saturating_duration_since(start)
}

pub fn use_fixed_step() {
    FIXED.with(|fixed| fixed.set(Some(Instant::now())));
}

pub fn advance() {
    FIXED.with(|fixed| fixed.set(fixed.get().map(|now| now + FIXED_STEP)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_step_ignores_wall_time() {
        use_fixed_step();
        let start = now();
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(since(start), Duration::ZERO);
        advance();
        advance();
        assert_eq!(since(start), FIXED_STEP * 2);
    }
}
