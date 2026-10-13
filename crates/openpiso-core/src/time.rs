/// Formats signed seconds as labelled hours, minutes, and seconds.
///
/// Leading zero units are omitted while the seconds unit is always included.
/// Hours do not roll over into days.
pub fn format_labelled_duration(seconds: i64) -> String {
    let sign = if seconds < 0 { "-" } else { "" };
    let magnitude = seconds.unsigned_abs();
    let hours = magnitude / 3_600;
    let minutes = magnitude / 60 % 60;
    let seconds = magnitude % 60;

    if hours > 0 {
        format!("{sign}{hours}h {minutes}min {seconds}sec")
    } else if minutes > 0 {
        format!("{sign}{minutes}min {seconds}sec")
    } else {
        format!("{sign}{seconds}sec")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_duration_in_labelled_units() {
        assert_eq!(format_labelled_duration(200), "3min 20sec");
        assert_eq!(format_labelled_duration(3750), "1h 2min 30sec");
        assert_eq!(format_labelled_duration(45), "45sec");
        assert_eq!(format_labelled_duration(60), "1min 0sec");
        assert_eq!(format_labelled_duration(3600), "1h 0min 0sec");
        assert_eq!(format_labelled_duration(100000), "27h 46min 40sec");
        assert_eq!(format_labelled_duration(0), "0sec");
        assert_eq!(format_labelled_duration(-200), "-3min 20sec");
    }
}
