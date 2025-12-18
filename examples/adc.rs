#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(proc_macro_hygiene)]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use esp_hal::analog::adc::{Adc, AdcCalBasic, AdcConfig, Attenuation};
use esp_hal::efuse::Efuse;

use defmt_rtt as _;
use esp_backtrace as _;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);

    let boot_time = rtc.time_since_boot().as_millis();
    let wakeup_cause = esp_hal::rtc_cntl::wakeup_cause();
    let delay = esp_hal::delay::Delay::new();

    defmt::info!("INIT: boot_time={} wakeup={}", boot_time, wakeup_cause);

    // Initialise ADC
    for (a, v) in [
        ("0dB", Attenuation::_0dB),
        ("2.5dB", Attenuation::_2p5dB),
        ("6dB", Attenuation::_6dB),
        ("11dB", Attenuation::_11dB),
    ] {
        let cal_mv = Efuse::rtc_calib_cal_mv(esp_hal::efuse::AdcCalibUnit::ADC1, v); // e.g. 2750 mV
        let cal_code =
            Efuse::rtc_calib_init_code(esp_hal::efuse::AdcCalibUnit::ADC1, v).unwrap() as u32;
        defmt::info!("{} :: CAL_MV = {} / CAL_CODE = {}", a, cal_mv, cal_code);
    }

    let adc_pin = peripherals.GPIO0;
    let mut adc_config = AdcConfig::new();
    let mut pin = adc_config.enable_pin_with_cal::<_, AdcCalBasic<_>>(adc_pin, Attenuation::_11dB);
    let mut adc = Adc::new(peripherals.ADC1, adc_config);

    loop {
        let adc_raw = adc.read_oneshot(&mut pin).unwrap() as u32;
        let adc_v = adc_raw as f32 * 1.07 / 1000.0; // 1.07mV per code
        defmt::info!("ADC Raw: {} / V: {}", adc_raw, adc_v);
        delay.delay_millis(200);
    }
}
