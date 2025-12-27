#![no_std]
#![no_main]
#![feature(type_alias_impl_trait)]
#![feature(proc_macro_hygiene)]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

extern crate alloc;

use esp_hal::analog::adc::{Adc, AdcCalBasic, AdcConfig, Attenuation};
use esp_hal::gpio::{DriveStrength, Level, Output, OutputConfig};
use esp_hal::i2c;
use esp_hal::rtc_cntl::sleep::TimerWakeupSource;
use esp_hal::system::SleepSource;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;

#[cfg(target_arch = "riscv32")]
use esp_hal::interrupt::software::SoftwareInterruptControl;

use esp_hal_utils::format_mac::format_mac;

use defmt_rtt as _;
use esp_backtrace as _;

use core::fmt::Write;

esp_bootloader_esp_idf::esp_app_desc!();

#[esp_hal::main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let mut rtc = esp_hal::rtc_cntl::Rtc::new(peripherals.LPWR);
    let timer = TimerWakeupSource::new(core::time::Duration::from_secs(10));

    let boot_time = rtc.time_since_boot().as_millis();
    let wakeup_cause = esp_hal::rtc_cntl::wakeup_cause();
    let delay = esp_hal::delay::Delay::new();

    defmt::info!("INIT: boot_time={} wakeup={}", boot_time, wakeup_cause);

    esp_alloc::heap_allocator!(size: 64 * 1024);

    #[cfg(target_arch = "riscv32")]
    let sw_int = SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    let timg0 = TimerGroup::new(peripherals.TIMG0);
    esp_rtos::start(
        timg0.timer0,
        #[cfg(target_arch = "riscv32")]
        sw_int.software_interrupt0,
    );

    defmt::info!("ESP_RTOS initialized!");

    if let SleepSource::Undefined = wakeup_cause {
        hub::clear_rtc();
        for i in 0..5 {
            defmt::info!("WAIT [{}]", i);
            delay.delay_millis(1000);
        }
    }

    #[cfg(feature = "esp32c6")]
    let mut led = Output::new(peripherals.GPIO15, Level::Low, OutputConfig::default());
    #[cfg(not(feature = "esp32c6"))]
    let mut led = Output::new(peripherals.GPIO1, Level::Low, OutputConfig::default());

    // Onboard LED
    led.set_high();

    // Initialise ESP_NOW
    defmt::info!("Initialise ESP_NOW");
    let esp_radio_ctrl = esp_radio::init().unwrap();
    let wifi = peripherals.WIFI;
    let (mut controller, interfaces) =
        esp_radio::wifi::new(&esp_radio_ctrl, wifi, Default::default()).unwrap();
    controller.set_mode(esp_radio::wifi::WifiMode::Sta).unwrap();
    controller.start().unwrap();

    let mut esp_now = interfaces.esp_now;
    esp_now.set_channel(11).unwrap();

    defmt::info!("ESP-NOW VERSION: {}", esp_now.version().unwrap());
    defmt::info!(
        "        MAC ADDRESS: {}",
        format_mac(&esp_radio::wifi::sta_mac())
    );

    let hub_address = hub::find_hub(&mut esp_now, delay.clone());

    // Initialise ADC
    let adc_pin = peripherals.GPIO0;
    let mut adc_config = AdcConfig::new();
    let mut pin = adc_config.enable_pin_with_cal::<_, AdcCalBasic<_>>(adc_pin, Attenuation::_11dB);
    let mut adc = Adc::new(peripherals.ADC1, adc_config);
    let adc_avg = (0..5)
        .map(|_| {
            let adc_raw = adc.read_oneshot(&mut pin).unwrap() as u32;
            // let adc_v = adc_raw as f32 * 1.07 / 1000.0; // 1.07mV per code point
            // XXX With 2 x 1M voltage divider ADC pin resistance is too high XXX
            let adc_v = 2.0 * adc_raw as f32 * 1.5 / 1000.0; // ~1.5mV per code point with 1M divider
                                                             // 2x Voltage Divider
            defmt::info!("Battery Voltage: {}V [{}]", adc_v, adc_raw);
            delay.delay_millis(5);
            adc_v
        })
        .sum::<f32>()
        / 5.0;

    let mut buf = heapless::String::<256>::new();

    write!(&mut buf, "[{}] C6-SENSOR: v={}V", boot_time / 1000, adc_avg).unwrap();

    let status = esp_now.send(&hub_address, buf.as_bytes()).unwrap().wait();
    defmt::info!("ESP-NOW: TX (blocking) -> {:?}", status);

    // delay.delay_millis(1000);
    defmt::info!("SLEEPING:");

    // Turn off I2C power
    rtc.sleep_deep(&[&timer]);
}

mod hub {

    use esp_hal_utils::crc::crc16;
    use esp_hal_utils::format_mac::format_mac;
    use esp_radio::esp_now::{EspNow, PeerInfo, BROADCAST_ADDRESS};

    // MAGIC = 2 bytes, ADDRESS = 6 bytes, CRC = 2 bytes
    #[derive(Clone, Copy)]
    #[repr(transparent)]
    pub struct HubAddressRtc([u8; 10]);

    unsafe impl esp_hal::Persistable for HubAddressRtc {}

    impl HubAddressRtc {
        fn pack(address: [u8; 6]) -> Self {
            let mut buf = [0_u8; 10];
            buf[0..2].copy_from_slice(&(0x1111_u16).to_le_bytes());
            buf[2..8].copy_from_slice(&address);
            let crc = crc16(&buf[0..8]).to_le_bytes();
            buf[8..10].copy_from_slice(&crc);
            Self(buf)
        }
        fn check(&self) -> bool {
            let crc = crc16(&self.0[0..8]).to_le_bytes();
            crc == self.0[8..10]
        }
        fn unpack(&self) -> Option<[u8; 6]> {
            if self.check() {
                let mut out = [0_u8; 6];
                out.copy_from_slice(&self.0[2..8]);
                Some(out)
            } else {
                None
            }
        }
    }

    #[esp_hal::ram(unstable(rtc_fast, persistent))]
    static mut HUB_ADDRESS: HubAddressRtc = HubAddressRtc([0_u8; 10]);

    pub fn clear_rtc() {
        unsafe { HUB_ADDRESS = HubAddressRtc([0_u8; 10]) }
    }

    pub fn find_hub(esp_now: &mut EspNow<'_>, delay: esp_hal::delay::Delay) -> [u8; 6] {
        let hub_rtc = unsafe { HUB_ADDRESS };
        match hub_rtc.unpack() {
            Some(hub) => {
                // Stored hub address
                defmt::info!("USING STORED HUB ADDRESS");
                defmt::info!("ESP-NOW ADD PEER: {}", format_mac(&hub));
                esp_now
                    .add_peer(PeerInfo {
                        interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                        peer_address: hub,
                        lmk: None,
                        channel: None,
                        encrypt: false,
                    })
                    .unwrap();
                hub
            }
            None => {
                defmt::info!("SEARCHING FOR HUB BROADCAST");
                loop {
                    if let Some(r) = esp_now.receive() {
                        if r.info.dst_address == BROADCAST_ADDRESS && r.data() == b"<<HUB>>" {
                            defmt::info!(">> RX HUB BROADCAST");
                            if !esp_now.peer_exists(&r.info.src_address) {
                                defmt::info!(
                                    "ESP-NOW ADD PEER: {}",
                                    format_mac(&r.info.src_address)
                                );
                                esp_now
                                    .add_peer(PeerInfo {
                                        interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                                        peer_address: r.info.src_address,
                                        lmk: None,
                                        channel: None,
                                        encrypt: false,
                                    })
                                    .unwrap();
                            }
                            unsafe { HUB_ADDRESS = HubAddressRtc::pack(r.info.src_address) };
                            break r.info.src_address;
                        }
                    }
                    delay.delay_millis(50);
                }
            }
        }
    }
}
