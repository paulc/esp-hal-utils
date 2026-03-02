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

use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
use embassy_executor::Spawner;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, mutex::Mutex};
use embassy_time::Timer;

use embedded_graphics::{
    mono_font::{ascii::FONT_6X10, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Primitive, PrimitiveStyle, Triangle},
    text::Text,
};

use lcd_async::{
    interface,
    models::ST7789,
    options::{ColorInversion, ColorOrder, Orientation, Rotation},
    raw_framebuf::RawFrameBuf,
    Builder, TestImage,
};

use defmt::info;

use static_cell::StaticCell;

// Display parameters
// const WIDTH: u16 = 172;
// const HEIGHT: u16 = 320;
const WIDTH: u16 = 320;
const HEIGHT: u16 = 172;
const PIXEL_SIZE: usize = 2; // RGB565 = 2 bytes per pixel
const FRAME_SIZE: usize = (WIDTH as usize) * (HEIGHT as usize) * PIXEL_SIZE;

static FRAME_BUFFER: StaticCell<[u8; FRAME_SIZE]> = StaticCell::new();

#[esp_rtos::main]
async fn main(_spawner: Spawner) {
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
            .with_frequency(Rate::from_mhz(40))
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
    let di = interface::SpiInterface::new(spi_device, dc);
    let mut delay = embassy_time::Delay;

    // Initialize the display
    let mut display = Builder::new(ST7789, di)
        .reset_pin(res)
        //.orientation(Orientation::default())
        .orientation(Orientation::default().rotate(Rotation::Deg270))
        .color_order(ColorOrder::Rgb)
        .invert_colors(ColorInversion::Inverted)
        .display_size(172, 320)
        .display_offset(34, 0)
        .init(&mut delay)
        .await
        .expect("Display Error");

    info!("Display initialized!");

    // Initialize frame buffer
    let frame_buffer = FRAME_BUFFER.init_with(|| [0; FRAME_SIZE]);

    {
        // Create a framebuffer in separate scope to ensure dropped
        let mut raw_fb =
            RawFrameBuf::<Rgb565, _>::new(frame_buffer.as_mut_slice(), WIDTH.into(), HEIGHT.into());

        // Clear the framebuffer to black
        raw_fb.clear(Rgb565::BLACK).unwrap();

        TestImage::new().draw(&mut raw_fb).unwrap();

        // Send the framebuffer data to the display
        info!("TEST IMAGE");
        display
            .show_raw_data(0, 0, WIDTH, HEIGHT, frame_buffer)
            .await
            .unwrap();
    }

    Timer::after_millis(5000).await;

    {
        // Create a framebuffer in separate scope to ensure dropped
        let mut raw_fb =
            RawFrameBuf::<Rgb565, _>::new(frame_buffer.as_mut_slice(), WIDTH.into(), HEIGHT.into());

        // Clear the framebuffer to black
        raw_fb.clear(Rgb565::BLACK).unwrap();

        draw_test(&mut raw_fb).unwrap();

        // Send the framebuffer data to the display
        display
            .show_raw_data(0, 0, WIDTH, HEIGHT, frame_buffer)
            .await
            .unwrap();
    }

    Timer::after_millis(5000).await;

    loop {
        for (n, c) in [
            ("RED", Rgb565::RED),
            ("BLUE", Rgb565::BLUE),
            ("GREEN", Rgb565::GREEN),
        ] {
            let mut raw_fb = RawFrameBuf::<Rgb565, _>::new(
                frame_buffer.as_mut_slice(),
                WIDTH.into(),
                HEIGHT.into(),
            );
            raw_fb.clear(c).ok();
            info!("FILL: {:?}", n);
            display
                .show_raw_data(0, 0, WIDTH, HEIGHT, frame_buffer)
                .await
                .unwrap();
            Timer::after_millis(1000).await;
        }
    }
}

fn draw_test<T>(display: &mut T) -> Result<(), T::Error>
where
    T: DrawTarget<Color = Rgb565>,
{
    // Draw an upside down triangle to represent a smiling mouth
    Triangle::new(Point::new(0, 0), Point::new(128, 0), Point::new(128, 128))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::RED))
        .draw(display)?;

    Triangle::new(Point::new(0, 160), Point::new(128, 160), Point::new(0, 32))
        .into_styled(PrimitiveStyle::with_fill(Rgb565::GREEN))
        .draw(display)?;

    // Create a new character style
    let style = MonoTextStyle::new(&FONT_6X10, Rgb565::WHITE);

    // Create a text at position (20, 30) and draw it using the previously defined style
    Text::new("Hello!", Point::new(20, 30), style).draw(display)?;

    Ok(())
}
