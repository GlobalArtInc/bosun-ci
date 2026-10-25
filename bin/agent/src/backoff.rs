use std::time::Duration;

const FIRST: Duration = Duration::from_secs(1);

const LONGEST: Duration = Duration::from_secs(60);

pub(crate) fn delay(failures: u32) -> Duration {
    jittered(failures, getrandom::u64().unwrap_or(u64::MAX))
}

fn jittered(failures: u32, random: u64) -> Duration {
    let nominal = FIRST
        .saturating_mul(2_u32.saturating_pow(failures))
        .min(LONGEST);
    let half = nominal / 2;
    let fraction = random as f64 / u64::MAX as f64;
    (half + half.mul_f64(fraction)).max(FIRST)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::jittered;

    #[test]
    fn delays_double_from_one_second_up_to_sixty_and_are_jittered_within_their_upper_half() {
        let bounds: Vec<(Duration, Duration)> = (0..10)
            .map(|failures| (jittered(failures, 0), jittered(failures, u64::MAX)))
            .collect();
        let seconds = |low: u64, high: u64| (Duration::from_secs(low), Duration::from_secs(high));
        assert_eq!(
            bounds,
            vec![
                seconds(1, 1),
                seconds(1, 2),
                seconds(2, 4),
                seconds(4, 8),
                seconds(8, 16),
                seconds(16, 32),
                seconds(30, 60),
                seconds(30, 60),
                seconds(30, 60),
                seconds(30, 60),
            ]
        );
        assert_eq!(jittered(u32::MAX, u64::MAX), Duration::from_secs(60));
        let middle = jittered(3, u64::MAX / 2);
        assert!(middle > Duration::from_secs(4) && middle < Duration::from_secs(8));
    }
}
