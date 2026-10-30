//! Decibel helpers shared by the mixer, the DSP nodes and the UI.

/// Converts a decibel value to a linear amplitude.
pub fn db_to_linear(db: f32) -> f32 {
    if db <= -144.0 {
        0.0
    } else {
        10f32.powf(db / 20.0)
    }
}

/// Converts a linear amplitude to decibels.
///
/// Silence maps to negative infinity.
pub fn linear_to_db(linear: f32) -> f32 {
    if linear <= 0.0 {
        f32::NEG_INFINITY
    } else {
        20.0 * linear.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unity_gain_is_zero_db() {
        assert!((db_to_linear(0.0) - 1.0).abs() < f32::EPSILON);
        assert!(linear_to_db(1.0).abs() < 1e-6);
    }

    #[test]
    fn minus_six_db_is_about_half() {
        assert!((db_to_linear(-6.0206) - 0.5).abs() < 1e-4);
    }

    #[test]
    fn silence_is_negative_infinity() {
        assert_eq!(linear_to_db(0.0), f32::NEG_INFINITY);
        assert_eq!(db_to_linear(-200.0), 0.0);
    }

    #[test]
    fn round_trip() {
        for db in [-60.0, -12.0, -3.0, 0.0, 6.0] {
            let round_tripped = linear_to_db(db_to_linear(db));
            assert!((round_tripped - db).abs() < 1e-4, "db = {db}");
        }
    }
}
