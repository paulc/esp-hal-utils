use esp_hal::gpio::Level;
use esp_hal::rmt::{Channel, PulseCode, Tx};
use esp_hal::Async;

use crate::rgb::{Rgb, RgbLayout};

use anyhow;

/*
    Ws2812
    ======

    static WS2812: StaticCell<ws2812::Ws2812<'static>> = StaticCell::new();

    // Create RMT Device
    let rmt = Rmt::new(peripherals.RMT, Rate::from_mhz(ws2812::APB_CLOCK_MHZ))
        .expect("Error initialising RMT")
        .into_async();

    // Set TX config
    let tx_config = TxChannelConfig::default()
        .with_clk_divider(ws2812::RMT_CLK_DIVIDER)
        .with_idle_output_level(Level::Low)
        .with_idle_output(true)
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

*/

pub const APB_CLOCK_MHZ: u32 = 80;
pub const RMT_CLK_DIVIDER: u8 = 2;

// WS2812 timings: 1us = RMT_FREQ / RMT_CLK_DIVIDER
const RMT_CHAN_FREQ: u16 = APB_CLOCK_MHZ as u16 / RMT_CLK_DIVIDER as u16;
const T0H: u16 = RMT_CHAN_FREQ * 400 / 1000; // 0.4us
const T0L: u16 = RMT_CHAN_FREQ * 850 / 1000; // 0.85us
const T1H: u16 = RMT_CHAN_FREQ * 800 / 1000; // 0.8us
const T1L: u16 = RMT_CHAN_FREQ * 450 / 1000; // 0.45us
const RESET_H: u16 = RMT_CHAN_FREQ * 500; // 500us

const T0: PulseCode = PulseCode::new(Level::High, T0H, Level::Low, T0L);
const T1: PulseCode = PulseCode::new(Level::High, T1H, Level::Low, T1L);
const RESET: PulseCode = PulseCode::new(Level::Low, RESET_H, Level::High, 0); // We can use RESET
                                                                              // as an end-marker
                                                                              // as length2 is 0

pub struct Ws2812<'a> {
    channel: Channel<'a, Async, Tx>,
    rgb_layout: RgbLayout,
}

const MAX_PIXELS: usize = 64;
const N_PIXEL: usize = 24;

impl<'a> Ws2812<'a> {
    pub fn new(channel: Channel<'a, Async, Tx>, rgb_layout: RgbLayout) -> Self {
        Self {
            channel,
            rgb_layout,
        }
    }
    pub async fn set_n(&mut self, pixels: &[Rgb]) -> anyhow::Result<()> {
        // Send pixel data in one transaction (upto MAX_PIXELS)
        let mut pulses = [PulseCode::default(); (MAX_PIXELS * N_PIXEL) + 1];
        let used = pixels.len() * N_PIXEL;
        for (i, c) in pixels.iter().enumerate() {
            let p = encode_pixel(c, self.rgb_layout);
            let start = i * 24;
            pulses[start..(start + 24)].copy_from_slice(&p);
        }
        pulses[used] = PulseCode::end_marker();

        let slice = &pulses[..=used]; // [0, used] inclusive → used+1 elements
        defmt::info!("Transmitting {} pulses", slice.len());

        // reset
        embassy_time::Timer::after_micros(60).await;
        self.channel.transmit(&slice).await.map_err(|e| {
            defmt::error!("RMT Error: {:?}", e);
            anyhow::anyhow!("RMT Error: {e}")
        })?;
        // reset
        embassy_time::Timer::after_micros(60).await;

        Ok(())
    }
    pub async fn set(&mut self, pixels: &[Rgb]) -> anyhow::Result<()> {
        // Initial reset
        let reset = [RESET, PulseCode::default()];
        self.channel.transmit(&reset).await.map_err(|e| {
            defmt::error!("RMT Error: {:?}", e);
            anyhow::anyhow!("RMT Error: {e}")
        })?;
        for p in pixels {
            // Generate pulses
            let pulses = encode_pixel_with_end_marker(p, self.rgb_layout);
            // Send data
            self.channel.transmit(&pulses).await.map_err(|e| {
                defmt::error!("RMT Error: {:?}", e);
                anyhow::anyhow!("RMT Error: {e}")
            })?;
        }
        // Send reset
        self.channel.transmit(&reset).await.map_err(|e| {
            defmt::error!("RMT Error: {:?}", e);
            anyhow::anyhow!("RMT Error: {e}")
        })
    }
}

fn encode_pixel(colour: &Rgb, layout: RgbLayout) -> [PulseCode; 24] {
    // Need to include end marker
    let mut pulses = [PulseCode::default(); 24];
    let c = colour.to_u32(layout);
    // Generate pulses
    #[allow(clippy::needless_range_loop)]
    for i in 0..24 {
        // Send MSB first
        let bit = (c >> (23 - i)) & 1;
        pulses[i] = if bit == 0 { T0 } else { T1 };
    }
    pulses
}

fn encode_pixel_with_end_marker(colour: &Rgb, layout: RgbLayout) -> [PulseCode; 25] {
    // Need to include end marker
    let mut pulses = [PulseCode::default(); 25];
    let c = colour.to_u32(layout);
    // Generate pulses
    #[allow(clippy::needless_range_loop)]
    for i in 0..24 {
        // Send MSB first
        let bit = (c >> (23 - i)) & 1;
        pulses[i] = if bit == 0 { T0 } else { T1 };
    }
    pulses
}
