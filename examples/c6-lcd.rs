#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

use esp_hal::{
    clock::CpuClock,
    dma::{DmaRxBuf, DmaTxBuf},
    gpio::{Level, Output},
    i2c,
    spi::{
        master::{Config, Spi, SpiDmaBus},
        Mode,
    },
    time::Rate,
    timer::timg::TimerGroup,
    Async,
};

#[cfg(target_arch = "riscv32")]
use esp_hal::interrupt::software::SoftwareInterruptControl;

use defmt_rtt as _;
use esp_backtrace as _;

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_executor::Spawner;
use embassy_sync::channel::{Channel, Receiver};
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::Timer;

use embedded_graphics::{
    mono_font::MonoTextStyle,
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Primitive, PrimitiveStyle, Triangle},
    text::Text,
};

use lcd_async::{
    interface::SpiInterface,
    models::ST7789,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
    raw_framebuf::RawFrameBuf,
    Builder, TestImage,
};

use core::fmt::Write;
use defmt::info;

use static_cell::StaticCell;

use esp_hal_utils::ina219;

// Display parameters
// const DISPLAY_WIDTH: u16 = 172;
// const DISPLAY_HEIGHT: u16 = 320;
const DISPLAY_WIDTH: u16 = 320;
const DISPLAY_HEIGHT: u16 = 172;
const PIXEL_SIZE: usize = 2; // RGB565 = 2 bytes per pixel
const FRAME_SIZE: usize = (DISPLAY_WIDTH as usize) * (DISPLAY_HEIGHT as usize) * PIXEL_SIZE;

static FRAME_BUFFER: StaticCell<[u8; FRAME_SIZE]> = StaticCell::new();
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

    // Create DMA buffers for SPI
    #[allow(clippy::manual_div_ceil)]
    let (rx_buffer, rx_descriptors, tx_buffer, tx_descriptors) = esp_hal::dma_buffers!(64, 32_000);
    let dma_rx_buf = DmaRxBuf::new(rx_descriptors, rx_buffer).unwrap();
    let dma_tx_buf = DmaTxBuf::new(tx_descriptors, tx_buffer).unwrap();

    let dc = peripherals.GPIO15; // DC (Data/Command)
    let cs = peripherals.GPIO14; // CS (Chip Select)
    let sclk = peripherals.GPIO7; // CLK
    let mosi = peripherals.GPIO6; // DIN
    let res = peripherals.GPIO21; // RES (Reset)
    let bl = peripherals.GPIO22; // Backlight

    // Create SPI with DMA
    let spi = Spi::new(
        peripherals.SPI2,
        Config::default()
            .with_frequency(Rate::from_mhz(80))
            .with_mode(Mode::_0),
    )
    .unwrap()
    .with_sck(sclk)
    .with_mosi(mosi)
    .with_dma(peripherals.DMA_CH0)
    .with_buffers(dma_rx_buf, dma_tx_buf)
    .into_async();

    // Create control pins
    let res = Output::new(res, Level::Low, Default::default());
    let dc = Output::new(dc, Level::Low, Default::default());
    let cs = Output::new(cs, Level::High, Default::default());

    // Turn on backlight
    let _bl = Output::new(bl, Level::High, Default::default());

    // Create shared SPI bus
    static SPI_BUS: StaticCell<Mutex<NoopRawMutex, SpiDmaBus<'static, Async>>> = StaticCell::new();
    let spi_bus = Mutex::new(spi);
    let spi_bus = SPI_BUS.init(spi_bus);
    let spi_device = SpiDevice::new(spi_bus, cs);

    // Create display interface
    let di = SpiInterface::new(spi_device, dc);
    let mut delay = embassy_time::Delay;

    // Initialize the display
    let display = Builder::new(ST7789, di)
        .reset_pin(res)
        .orientation(Orientation::default().rotate(Rotation::Deg270))
        .color_order(ColorOrder::Rgb)
        .invert_colors(ColorInversion::Inverted)
        .display_size(DISPLAY_HEIGHT, DISPLAY_WIDTH) // XXX Inverted??
        .display_offset(34, 0)
        .init(&mut delay)
        .await
        .expect("Display Error");

    info!("Display initialized!");

    // Create LCD task
    let lcd_channel = LCD_CHANNEL.init(Channel::new());
    let lcd_rx = LCD_CHANNEL_RX.init(lcd_channel.receiver());
    let lcd_tx = lcd_channel.sender();
    spawner.spawn(lcd_task(display, lcd_rx)).unwrap();

    lcd_tx.send((LcdMessage::TestImage, true)).await;
    Timer::after_millis(200).await;

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

    let (brng, pga, badc, sadc) = ina219_device.read_config().await.unwrap().as_str();
    defmt::info!(
        "INA219 Config: Brng: {} / PGA: {} / BADC: {} / SADC: {}",
        brng,
        pga,
        badc,
        sadc
    );

    ina219_device.reset().await.unwrap();
    let new_config = ina219::Ina219Config::default()
        .with_brng(ina219::Ina219Brng::Brng32V)
        .with_pga(ina219::Ina219Pga::Pga320mV)
        .with_badc(ina219::Ina219Adc::Adc12_64)
        .with_sadc(ina219::Ina219Adc::Adc12_64);
    ina219_device.write_config(new_config).await.unwrap();

    let (brng, pga, badc, sadc) = ina219_device.read_config().await.unwrap().as_str();
    defmt::info!(
        "INA219 Config: Brng: {} / PGA: {} / BADC: {} / SADC: {}",
        brng,
        pga,
        badc,
        sadc
    );

    loop {
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
        Timer::after_millis(50).await;
    }
}

pub enum LcdMessage {
    TestImage,
    Background(Rgb565),
    Draw,
    Text(heapless::String<40>, Point, u8, Rgb565), // Text, (x,y), font_size, colour
    Static(&'static str, Point, u8, Rgb565),       // Text, (x,y), font_size, colour
    Scroll(heapless::String<40>),
}

const FONT_HEIGHT: u16 = 16;
const DISPLAY_LINES: usize = (DISPLAY_HEIGHT / FONT_HEIGHT) as usize;

static LCD_CHANNEL: StaticCell<Channel<NoopRawMutex, (LcdMessage, bool), 1>> = StaticCell::new();
static LCD_CHANNEL_RX: StaticCell<Receiver<NoopRawMutex, (LcdMessage, bool), 1>> =
    StaticCell::new();

type LcdSpiDevice = SpiDevice<'static, NoopRawMutex, SpiDmaBus<'static, Async>, Output<'static>>;
type LcdDisplay =
    lcd_async::Display<SpiInterface<LcdSpiDevice, Output<'static>>, ST7789, Output<'static>>;

#[embassy_executor::task]
async fn lcd_task(
    mut display: LcdDisplay,
    lcd_rx: &'static mut Receiver<'static, NoopRawMutex, (LcdMessage, bool), 1>,
) {
    // Initialize frame buffer
    let frame_buffer = FRAME_BUFFER.init_with(|| [0; FRAME_SIZE]);
    let mut lines = heapless::Deque::<heapless::String<40>, DISPLAY_LINES>::new();
    loop {
        let mut raw_fb = RawFrameBuf::<Rgb565, _>::new(
            frame_buffer.as_mut_slice(),
            DISPLAY_WIDTH.into(),
            DISPLAY_HEIGHT.into(),
        );
        let (msg, update) = lcd_rx.receive().await;
        match msg {
            LcdMessage::Draw => {} // Empty command to allow update
            LcdMessage::TestImage => {
                TestImage::new().draw(&mut raw_fb).unwrap();
            }
            LcdMessage::Background(c) => {
                raw_fb.clear(c).ok();
            }
            LcdMessage::Text(t, p, s, c) => {
                let style = match s {
                    7 => MonoTextStyle::new(&profont::PROFONT_7_POINT, c),
                    9 => MonoTextStyle::new(&profont::PROFONT_9_POINT, c),
                    12 => MonoTextStyle::new(&profont::PROFONT_12_POINT, c),
                    14 => MonoTextStyle::new(&profont::PROFONT_14_POINT, c),
                    18 => MonoTextStyle::new(&profont::PROFONT_18_POINT, c),
                    24 => MonoTextStyle::new(&profont::PROFONT_24_POINT, c),
                    _ => MonoTextStyle::new(&profont::PROFONT_14_POINT, c), // Default
                };
                Text::new(t.as_str(), p, style).draw(&mut raw_fb).unwrap();
            }
            LcdMessage::Static(t, p, s, c) => {
                let style = match s {
                    7 => MonoTextStyle::new(&profont::PROFONT_7_POINT, c),
                    9 => MonoTextStyle::new(&profont::PROFONT_9_POINT, c),
                    12 => MonoTextStyle::new(&profont::PROFONT_12_POINT, c),
                    14 => MonoTextStyle::new(&profont::PROFONT_14_POINT, c),
                    18 => MonoTextStyle::new(&profont::PROFONT_18_POINT, c),
                    24 => MonoTextStyle::new(&profont::PROFONT_24_POINT, c),
                    _ => MonoTextStyle::new(&profont::PROFONT_14_POINT, c), // Default
                };
                Text::new(t, p, style).draw(&mut raw_fb).unwrap();
            }
            LcdMessage::Scroll(t) => {
                raw_fb.clear(Rgb565::BLUE).ok();
                let style = MonoTextStyle::new(&profont::PROFONT_14_POINT, Rgb565::WHITE);
                if lines.is_full() {
                    lines.pop_front().expect("pop_back");
                }
                lines.push_back(t).expect("push_front");

                for (i, l) in lines.iter().enumerate() {
                    Text::new(
                        l.as_str(),
                        Point::new(10, (FONT_HEIGHT * (i + 1) as u16) as i32),
                        style,
                    )
                    .draw(&mut raw_fb)
                    .expect("text");
                }
            }
        }
        if update {
            display
                .show_raw_data(0, 0, DISPLAY_WIDTH, DISPLAY_HEIGHT, frame_buffer)
                .await
                .unwrap();
        }
    }
}

fn _draw_test<T>(display: &mut T) -> Result<(), T::Error>
where
    T: DrawTarget<Color = Rgb565>,
{
    Triangle::new(Point::new(0, 0), Point::new(128, 0), Point::new(128, 128))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::RED))
        .draw(display)?;

    Triangle::new(Point::new(0, 160), Point::new(128, 160), Point::new(0, 32))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::GREEN))
        .draw(display)?;

    let style = MonoTextStyle::new(&profont::PROFONT_24_POINT, Rgb565::WHITE);
    Text::new("Hello!", Point::new(20, 30), style).draw(display)?;

    Ok(())
}
