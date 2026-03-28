#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use esp_hal::{
    clock::CpuClock,
    gpio::{Input, InputConfig, Pull},
    i2c,
    time::Rate,
    timer::timg::TimerGroup,
    Async,
};

#[cfg(target_arch = "riscv32")]
use esp_hal::interrupt::software::SoftwareInterruptControl;

use defmt_rtt as _;
use esp_backtrace as _;

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::Spawner;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Duration, Ticker, Timer};

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::{Point, RgbColor},
};

use core::fmt::Write;
use defmt::info;

use static_cell::StaticCell;

use esp_hal_utils::c6_lcd;
use esp_hal_utils::ina219;

static I2C_BUS: StaticCell<Mutex<NoopRawMutex, i2c::master::I2c<Async>>> = StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(size: 32 * 1024);

    defmt::info!("Init!");

    #[cfg(target_arch = "riscv32")]
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    let timg0 = TimerGroup::new(peripherals.TIMG0);

    esp_rtos::start(
        timg0.timer0,
        #[cfg(target_arch = "riscv32")]
        sw_int.software_interrupt0,
    );

    info!("Embassy initialized!");

    // Init LCD (pass peripherals)
    let lcd_tx = c6_lcd::init_lcd(
        peripherals.GPIO15,  // DC (Data/Command)
        peripherals.GPIO14,  // CS (Chip Select)
        peripherals.GPIO7,   // CLK
        peripherals.GPIO6,   // DIN
        peripherals.GPIO21,  // RES (Reset)
        peripherals.GPIO22,  // Backlight
        peripherals.SPI2,    // SPI Device
        peripherals.DMA_CH0, // DMA device
        spawner.clone(),
    )
    .await
    .unwrap();

    lcd_tx
        .send((c6_lcd::LcdMessage::Background(Rgb565::RED), true))
        .await;

    // Initialise I2C Bus
    let i2c_config = i2c::master::Config::default().with_frequency(Rate::from_khz(100));
    let scl = peripherals.GPIO4;
    let sda = peripherals.GPIO5;
    let mut i2c = i2c::master::I2c::new(peripherals.I2C0, i2c_config)
        .expect("Error initailising I2C")
        .with_scl(scl)
        .with_sda(sda)
        .into_async();

    defmt::info!("Scan I2C bus: START");
    for addr in 0..=127 {
        if let Ok(_) = i2c.write_async(addr, &[0]).await {
            defmt::info!("Found I2C device at address: 0x{:02x}", addr);
        }
        Timer::after_millis(5).await;
    }

    defmt::info!("Scan I2C bus: DONE");
    // Create shared I2C bus
    let i2c_bus = I2C_BUS.init(Mutex::new(i2c));

    let mut ina219_device = ina219::Ina219::new(
        I2cDevice::new(i2c_bus),
        ina219::INA219_ADDRESS,
        ina219::INA219_SHUNT_RESISTOR,
    );

    ina219_device.reset().await.unwrap();

    ina219_device
        .write_config(
            ina219::Ina219Config::default()
                .with_brng(ina219::Ina219Brng::Brng32V)
                .with_pga(ina219::Ina219Pga::Pga80mV)
                .with_badc(ina219::Ina219Adc::Adc12_16)
                .with_sadc(ina219::Ina219Adc::Adc12_16),
        )
        .await
        .unwrap();

    let (brng, pga, badc, sadc) = ina219_device.read_config().await.unwrap().as_str();
    defmt::info!(
        "INA219 Config: Brng: {} / PGA: {} / BADC: {} / SADC: {}",
        brng,
        pga,
        badc,
        sadc
    );

    // Rotary encoder
    let mut enc_clk = Input::new(
        peripherals.GPIO9,
        InputConfig::default().with_pull(Pull::Up),
    );
    let enc_dt = Input::new(
        peripherals.GPIO18,
        InputConfig::default().with_pull(Pull::Up),
    );
    let mut enc_sw = Input::new(
        peripherals.GPIO19,
        InputConfig::default().with_pull(Pull::Up),
    );

    use embassy_futures::select::{select, Either};
    let mut counter = 0_i32;
    loop {
        match select(enc_clk.wait_for_any_edge(), enc_sw.wait_for_falling_edge()).await {
            Either::First(_) => {
                counter += match (enc_clk.is_high(), enc_dt.is_high()) {
                    (true, false) | (false, true) => 1,
                    (true, true) | (false, false) => -1,
                };
                defmt::info!("Counter: {}", counter);
            }
            Either::Second(_) => {
                defmt::info!("Button Press");
            }
        }
    }

    /*
    let mut ticker = Ticker::every(Duration::from_millis(100));

    loop {
        defmt::info!(
            "SW >> {} / DT >> {} / CLK >> {}",
            enc_sw.is_high(),
            enc_dt.is_high(),
            enc_clk.is_high()
        );

        // Update display
        lcd_tx
            .send((LcdMessage::Background(Rgb565::BLUE), false))
            .await;
        lcd_tx
            .send((
                LcdMessage::Static("Vbus", Point::new(10, 20), 14, Rgb565::WHITE),
                false,
            ))
            .await;
        lcd_tx
            .send((
                LcdMessage::Static("Ishunt", Point::new(170, 20), 14, Rgb565::WHITE),
                false,
            ))
            .await;

        let mut reading = ina219::Ina219Reading {
            bus_v: 0.0,
            shunt_ma: 0.0,
        };
        match ina219_device.read().await {
            Ok(r) => {
                reading = r;
            }
            Err(ina219::Ina219Error::NotReady) => {
                lcd_tx
                    .send((
                        LcdMessage::Static("Not Ready", Point::new(10, 100), 14, Rgb565::RED),
                        false,
                    ))
                    .await;
            }
            Err(ina219::Ina219Error::Overflow) => {
                lcd_tx
                    .send((
                        LcdMessage::Static("Overflow", Point::new(10, 100), 14, Rgb565::RED),
                        false,
                    ))
                    .await;
            }
            Err(ina219::Ina219Error::I2cError) => {
                lcd_tx
                    .send((
                        LcdMessage::Static("I2C Error", Point::new(10, 100), 14, Rgb565::RED),
                        false,
                    ))
                    .await;
            }
        }
        // Always display reading
        let mut v_txt = heapless::String::<40>::new();
        let _ = write!(&mut v_txt, "{:>7.3}V", reading.bus_v);
        lcd_tx
            .send((
                LcdMessage::Text(v_txt, Point::new(10, 80), 24, Rgb565::WHITE),
                false,
            ))
            .await;
        let mut i_txt = heapless::String::<40>::new();
        let _ = write!(&mut i_txt, "{:>7.3}mA", reading.shunt_ma);
        lcd_tx
            .send((
                LcdMessage::Text(i_txt, Point::new(170, 80), 24, Rgb565::WHITE),
                true, // Last message - update display
            ))
            .await;

        // Wait for next tick
        ticker.next().await
    }
    */
}
