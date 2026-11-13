use pressa_core::{Effort, ErrorKind};
use serde::{Deserialize, Serialize};

/// UI language.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lang {
    En,
    Ru,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Ru];

    /// Russian if the system locale is Russian, English otherwise.
    pub fn system() -> Self {
        match sys_locale::get_locale() {
            Some(l) if l.to_ascii_lowercase().starts_with("ru") => Lang::Ru,
            _ => Lang::En,
        }
    }

    /// Name of the language in that language.
    pub fn native_name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::Ru => "Русский",
        }
    }

    /// Picks the string for this language.
    pub fn t(self, en: &'static str, ru: &'static str) -> &'static str {
        match self {
            Lang::En => en,
            Lang::Ru => ru,
        }
    }

    /// "1 file / 2 files", "1 файл / 2 файла / 5 файлов".
    pub fn count(self, n: usize, en: [&str; 2], ru: [&str; 3]) -> String {
        let word = match self {
            Lang::En => en[usize::from(n != 1)],
            Lang::Ru => {
                let (m10, m100) = (n % 10, n % 100);
                if m10 == 1 && m100 != 11 {
                    ru[0]
                } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                    ru[1]
                } else {
                    ru[2]
                }
            }
        };
        format!("{n} {word}")
    }

    pub fn files(self, n: usize) -> String {
        self.count(n, ["file", "files"], ["файл", "файла", "файлов"])
    }

    pub fn effort(self, e: Effort) -> &'static str {
        match e {
            Effort::Fast => self.t("Fast", "Быстро"),
            Effort::Balanced => self.t("Balanced", "Сбалансированно"),
            Effort::Max => self.t("Max compression", "Макс. сжатие"),
        }
    }

    pub fn error(self, kind: Option<ErrorKind>) -> &'static str {
        match kind {
            Some(ErrorKind::UnsupportedFormat) => self.t("Unsupported format", "Формат не поддерживается"),
            Some(ErrorKind::ReadFailed) => self.t("Can't read the file", "Не удалось прочитать файл"),
            Some(ErrorKind::DecodeFailed) => self.t("Damaged or unsupported file", "Файл повреждён или не поддерживается"),
            Some(ErrorKind::EncodeFailed) => self.t("Encoding failed", "Ошибка сжатия"),
            Some(ErrorKind::WriteFailed) => self.t("Can't save the result", "Не удалось сохранить результат"),
            Some(ErrorKind::FfmpegMissing) => self.t("FFmpeg not found", "FFmpeg не найден"),
            Some(ErrorKind::VideoFailed) => self.t("Video compression failed", "Ошибка сжатия видео"),
            Some(ErrorKind::Cancelled) => self.t("Cancelled", "Отменено"),
            None => self.t("Error", "Ошибка"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn russian_plurals() {
        let f = |n| Lang::Ru.files(n);
        assert_eq!(f(1), "1 файл");
        assert_eq!(f(3), "3 файла");
        assert_eq!(f(5), "5 файлов");
        assert_eq!(f(11), "11 файлов");
        assert_eq!(f(21), "21 файл");
        assert_eq!(f(112), "112 файлов");
        assert_eq!(Lang::En.files(1), "1 file");
        assert_eq!(Lang::En.files(2), "2 files");
    }
}
