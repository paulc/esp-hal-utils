#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use esp_hal::timer::timg::TimerGroup;
use esp_hal::usb_serial_jtag::UsbSerialJtag;

#[cfg(target_arch = "riscv32")]
use esp_hal::interrupt::software::SoftwareInterruptControl;

use defmt_rtt as _;
use esp_backtrace as _;

use embassy_executor::Spawner;
use embassy_time::{Duration, Timer};

use esp_hal_utils::serial::frame_reader_task;

extern crate alloc;

// This creates a default app-descriptor required by the esp-idf bootloader.
// For more information see: <https://docs.espressif.com/projects/esp-idf/en/stable/esp32/api-reference/system/app_image_format.html#application-description>
esp_bootloader_esp_idf::esp_app_desc!();

#[embassy_executor::task]
async fn run() {
    let mut count = 0;
    loop {
        defmt::info!("[{}] Hello world from embassy!", count);
        Timer::after(Duration::from_millis(2_000)).await;
        count += 1;
    }
}

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    esp_alloc::heap_allocator!(size: 64 * 1024);

    defmt::info!("Init!");

    #[cfg(target_arch = "riscv32")]
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(
        timg0.timer0,
        #[cfg(target_arch = "riscv32")]
        sw_int.software_interrupt0,
    );

    // Start serial
    let (rx, _tx) = UsbSerialJtag::new(peripherals.USB_DEVICE)
        .into_async()
        .split();

    spawner.spawn(frame_reader_task(rx)).unwrap();
    spawner.spawn(run()).ok();

    loop {
        defmt::info!("Bing!");
        Timer::after(Duration::from_millis(5_000)).await;
    }
}
