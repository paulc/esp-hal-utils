#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use esp_hal::{clock::CpuClock, gpio::Pin, i2c, time::Rate, timer::timg::TimerGroup, Async};

#[cfg(target_arch = "riscv32")]
use esp_hal::interrupt::software::SoftwareInterruptControl;

use defmt_rtt as _;
use esp_backtrace as _;

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::Spawner;
use embassy_futures::select::{select, Either};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::{Duration, Ticker, Timer};

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::{Point, RgbColor},
};

use core::fmt::Write;
use defmt::info;

use static_cell::StaticCell;

use esp_hal_utils::c6_lcd::{init_lcd, LcdMessage, LcdSender};
use esp_hal_utils::encoder::{self, EncoderMsg};
use esp_hal_utils::ina219;

use portable_atomic::{AtomicI32, Ordering};

static I2C_BUS: StaticCell<Mutex<NoopRawMutex, i2c::master::I2c<Async>>> = StaticCell::new();
static ENCODER: AtomicI32 = AtomicI32::new(0);

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
    let mut lcd_tx = init_lcd(
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

    let (brng, pga, badc, sadc) = ina219_device.config.as_str();
    defmt::info!(
        "INA219 Config: Brng: {} / PGA: {} / BADC: {} / SADC: {}",
        brng,
        pga,
        badc,
        sadc
    );

    let encoder_rx = encoder::init(
        spawner.clone(),
        peripherals.GPIO18.degrade(),
        peripherals.GPIO19.degrade(),
        peripherals.GPIO20.degrade(),
    );

    let mut ticker = Ticker::every(Duration::from_millis(100));
    let mut reading = ina219::Ina219Reading {
        bus_v: 0.0,
        shunt_ma: 0.0,
    };
    let mut avg = heapless::Deque::<ina219::Ina219Reading, 10>::new();

    loop {
        match select(ticker.next(), encoder_rx.receive()).await {
            Either::First(_) => {
                // Update display
                reading = update_display(
                    &mut lcd_tx,
                    reading.clone(),
                    ina219_device.read().await,
                    &ina219_device.config.as_str(),
                )
                .await;
            }
            Either::Second(msg) => match msg {
                // Handle encoder input
                // - turn to increment/decrement selection
                // - press to cycle setting
                EncoderMsg::Button => {
                    // Get encoder_index: -1 not selected
                    let encoder_index = ENCODER.load(Ordering::Relaxed).rem_euclid(5) - 1;
                    let config = ina219_device.config.clone();
                    match encoder_index {
                        0 => {
                            // BRNG
                            ina219_device
                                .write_config(config.with_brng(config.get_brng().cycle()))
                                .await
                                .unwrap();
                        }
                        1 => {
                            // PGA
                            ina219_device
                                .write_config(config.with_pga(config.get_pga().cycle()))
                                .await
                                .unwrap();
                        }
                        2 => {
                            // BADC
                            ina219_device
                                .write_config(config.with_badc(config.get_badc().cycle()))
                                .await
                                .unwrap();
                        }
                        3 => {
                            // SADC
                            ina219_device
                                .write_config(config.with_sadc(config.get_sadc().cycle()))
                                .await
                                .unwrap();
                        }
                        _ => {}
                    }
                }
                EncoderMsg::Increment => {
                    ENCODER.fetch_add(1, Ordering::Relaxed);
                }
                EncoderMsg::Decrement => {
                    ENCODER.fetch_add(-1, Ordering::Relaxed);
                }
            },
        }
        if avg.is_full() {
            let (v, i) = avg.iter().fold((0.0, 0.0), |a, e| {
                (a.0 + e.bus_v, a.1 + e.shunt_ma / 1000.0)
            });
            defmt::info!(
                "{{ \"v\": {}, \"i\": {} }}",
                v / avg.len() as f32,
                i / avg.len() as f32
            );
            avg.clear();
        }
        avg.push_back(reading.clone()).unwrap(); // SAFE
    }
}

async fn update_display(
    lcd_tx: &mut LcdSender,
    previous: ina219::Ina219Reading,
    reading: Result<ina219::Ina219Reading, ina219::Ina219Error>,
    status: &(&str, &str, &str, &str),
) -> ina219::Ina219Reading {
    let mut next = previous;

    // Clear screen
    lcd_tx
        .send((LcdMessage::Background(Rgb565::BLUE), false))
        .await;

    // Update reading (checking for errors)
    match reading {
        Ok(r) => {
            next = r;
        }
        Err(ina219::Ina219Error::NotReady) => {
            lcd_tx
                .send((
                    LcdMessage::Static("Not Ready", Point::new(10, 105), 14, Rgb565::RED),
                    false,
                ))
                .await;
        }
        Err(ina219::Ina219Error::Overflow) => {
            lcd_tx
                .send((
                    LcdMessage::Static("Overflow", Point::new(10, 105), 14, Rgb565::RED),
                    false,
                ))
                .await;
        }
        Err(ina219::Ina219Error::I2cError) => {
            lcd_tx
                .send((
                    LcdMessage::Static("I2C Error", Point::new(10, 105), 14, Rgb565::RED),
                    false,
                ))
                .await;
        }
    }

    // Display reading
    let mut v_txt = heapless::String::<40>::new();
    let _ = write!(&mut v_txt, "{:>8.3}V", next.bus_v);
    lcd_tx
        .send((
            LcdMessage::Text(v_txt, Point::new(140, 25), 24, Rgb565::GREEN),
            false,
        ))
        .await;
    let mut i_txt = heapless::String::<40>::new();
    let _ = write!(&mut i_txt, "{:>8.3}mA", next.shunt_ma);
    lcd_tx
        .send((
            LcdMessage::Text(i_txt, Point::new(140, 55), 24, Rgb565::YELLOW),
            false,
        ))
        .await;
    let mut p_txt = heapless::String::<40>::new();
    let _ = write!(&mut p_txt, "{:>8.3}mW", next.bus_v * next.shunt_ma);
    lcd_tx
        .send((
            LcdMessage::Text(p_txt, Point::new(140, 85), 24, Rgb565::WHITE),
            false,
        ))
        .await;

    // Write Labels
    for (i, label) in ["V(bus)", "I(shunt)", "Power"].iter().enumerate() {
        lcd_tx
            .send((
                LcdMessage::Static(
                    label,
                    Point::new(10, 23 + (i as i32 * 30)),
                    18,
                    Rgb565::GREEN,
                ),
                false,
            ))
            .await;
    }

    // Write Settings
    // Get encoder position to highlight selected (-1 is not selected)
    let encoder_index = ENCODER.load(Ordering::Relaxed).rem_euclid(5) - 1;
    let (brng, pga, badc, sadc) = status;
    for (i, (label, value)) in [
        ("Range", brng),
        ("PGA", pga),
        ("BADC", badc),
        ("SADC", sadc),
    ]
    .iter()
    .enumerate()
    {
        let mut status_txt = heapless::String::<40>::new();
        let _ = write!(&mut status_txt, "{:<8}: {}", label, value);
        lcd_tx
            .send((
                LcdMessage::Text(
                    status_txt,
                    Point::new(10, 124 + (i as i32 * 14)),
                    12,
                    if encoder_index == i as i32 {
                        Rgb565::RED
                    } else {
                        Rgb565::GREEN
                    },
                ),
                false,
            ))
            .await;
    }

    // Update display
    lcd_tx.send((LcdMessage::Draw, true)).await;

    // Return new reading
    next
}
