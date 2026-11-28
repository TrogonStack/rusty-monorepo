use crate::constants::SECONDS_PER_DAY;

pub(crate) struct CivilDateTime {
    pub(crate) year: i64,
    pub(crate) month: i64,
    pub(crate) day: i64,
    pub(crate) hour: i64,
    pub(crate) minute: i64,
    pub(crate) second: i64,
}

impl CivilDateTime {
    pub(crate) fn from_unix_seconds(seconds: i64) -> Self {
        let days = seconds.div_euclid(SECONDS_PER_DAY);
        let remainder = seconds.rem_euclid(SECONDS_PER_DAY);
        let shifted = days + 719_468;
        let era = shifted.div_euclid(146_097);
        let day_of_era = shifted - era * 146_097;
        let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_index = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * month_index + 2) / 5 + 1;
        let month = if month_index < 10 {
            month_index + 3
        } else {
            month_index - 9
        };
        let year = year_of_era + era * 400 + i64::from(month <= 2);
        Self {
            year,
            month,
            day,
            hour: remainder / 3_600,
            minute: remainder % 3_600 / 60,
            second: remainder % 60,
        }
    }
}
