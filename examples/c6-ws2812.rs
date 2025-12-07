#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use esp_hal::gpio::Level;
use esp_hal::rmt::{Rmt, TxChannelConfig, TxChannelCreator};
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;

#[cfg(target_arch = "riscv32")]
use esp_hal::interrupt::software::SoftwareInterruptControl;

use defmt_rtt as _;
use esp_backtrace as _;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};

use static_cell::StaticCell;

use esp_hal_utils::rgb::{colour, Rgb, RgbLayout};
use esp_hal_utils::ws2812;

extern crate alloc;

static WS2812: StaticCell<ws2812::Ws2812<'static>> = StaticCell::new();

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[embassy_executor::task]
async fn run() {
    let mut count = 0;
    loop {
        defmt::info!("[{}] Hello world from embassy!", count);
        Timer::after(Duration::from_millis(1_000)).await;
        count += 1;
    }
}

#[embassy_executor::task]
async fn led_task(ws2812: &'static mut ws2812::Ws2812<'static>) {
    loop {
        for (msg, c) in &[
            ("red", colour::RED),
            ("green", colour::GREEN),
            ("blue", colour::BLUE),
            ("white", colour::WHITE),
            ("off", Rgb::new(0, 0, 0)),
        ] {
            defmt::info!(">> LED: {}", msg);
            ws2812.set(&[*c]).await.unwrap();
            Timer::after(Duration::from_millis(1_000)).await;
        }
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    esp_alloc::heap_allocator!(size: 64 * 1024);

    let peripherals = esp_hal::init(esp_hal::Config::default());
    let rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);

    let boot_time = rtc.time_since_boot().as_millis();
    let wakeup_cause = esp_hal::rtc_cntl::wakeup_cause();

    defmt::info!("INIT: boot_time={} wakeup={}", boot_time, wakeup_cause);

    #[cfg(target_arch = "riscv32")]
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(
        timg0.timer0,
        #[cfg(target_arch = "riscv32")]
        sw_int.software_interrupt0,
    );

    let rmt = Rmt::new(peripherals.RMT, Rate::from_mhz(ws2812::APB_CLOCK_MHZ))
        .expect("Error initialising RMT")
        .into_async();

    // Set TX config
    let tx_config = TxChannelConfig::default()
        .with_clk_divider(ws2812::RMT_CLK_DIVIDER)
        .with_idle_output(true)
        .with_idle_output_level(Level::Low)
        .with_carrier_modulation(false);

    // Create channel
    #[cfg(feature = "esp32c6")]
    let gpio = peripherals.GPIO8;
    #[cfg(feature = "esp32c3")]
    let gpio = peripherals.GPIO10;

    let channel = rmt
        .channel0
        .configure_tx(&tx_config)
        .unwrap()
        .with_pin(gpio);

    // Pass to WS2812 driver
    let ws2812 = ws2812::Ws2812::new(channel, RgbLayout::Grb);
    let ws2812_static = WS2812.init(ws2812);

    spawner.spawn(led_task(ws2812_static)).ok();
    spawner.spawn(run()).ok();

    loop {
        defmt::info!("Tick");
        Timer::after(Duration::from_millis(5_000)).await;
    }
}
