#[derive(PartialEq, Eq, Debug, defmt::Format)]
pub enum AppError {
    EncodingError,
    CapacityError,
    EspNowError,
}

impl core::fmt::Display for AppError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EncodingError => write!(f, "encoding failed"),
            Self::CapacityError => write!(f, "buffer capacity exceeded"),
            Self::EspNowError => write!(f, "ESP-NOW error"),
        }
    }
}

impl core::error::Error for AppError {}
