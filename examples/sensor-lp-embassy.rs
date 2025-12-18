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

use esp_hal::analog::adc::{Adc, AdcCalBasic, AdcConfig, AdcPin, Attenuation};
use esp_hal::gpio::{Level, Output, OutputConfig};
use esp_hal::i2c;
use esp_hal::interrupt::software::SoftwareInterruptControl;
use esp_hal::rtc_cntl::Rtc;
use esp_hal::system::SleepSource;
use esp_hal::time::Rate;
use esp_hal::timer::timg::TimerGroup;
use esp_hal::Async;

#[cfg(any(feature = "esp32s3"))]
use esp_hal::gpio::RtcPin as RtcIoWakeupPinType;
#[cfg(any(feature = "esp32c3", feature = "esp32c6"))]
use esp_hal::gpio::RtcPinWithResistors as RtcIoWakeupPinType;
#[cfg(feature = "esp32c6")]
use esp_hal::rtc_cntl::sleep::{Ext1WakeupSource, TimerWakeupSource, WakeupLevel};
#[cfg(any(feature = "esp32c3", feature = "esp32s3"))]
use esp_hal::rtc_cntl::sleep::{RtcioWakeupSource, TimerWakeupSource, WakeupLevel};
use esp_radio::esp_now::{EspNow, PeerInfo, BROADCAST_ADDRESS};

use embassy_embedded_hal::shared_bus::asynch::i2c::I2cDevice;
use embassy_executor::Spawner;
use embassy_futures::join::join3;
use embassy_sync::blocking_mutex::raw::{CriticalSectionRawMutex, NoopRawMutex};
use embassy_sync::mutex::Mutex;
use embassy_sync::signal::Signal;
use embassy_time::{with_timeout, Duration, TimeoutError, Timer};

use defmt_rtt as _;
use esp_backtrace as _;

use core::fmt::Write;
use static_cell::StaticCell;

use esp_hal_utils::aht20;
use esp_hal_utils::bmp280;
use esp_hal_utils::format_mac::format_mac;

#[esp_hal::ram(unstable(rtc_fast, persistent))]
static mut HUB_ADDRESS: [u8; 6] = [0; 6];

// This creates a default app-descriptor required by the esp-idf bootloader.
esp_bootloader_esp_idf::esp_app_desc!();

static PIN: StaticCell<
    AdcPin<
        esp_hal::peripherals::GPIO0<'_>,
        esp_hal::peripherals::ADC1<'_>,
        AdcCalBasic<esp_hal::peripherals::ADC1<'_>>,
    >,
> = StaticCell::new();

static I2C_BUS: StaticCell<Mutex<NoopRawMutex, i2c::master::I2c<Async>>> = StaticCell::new();
static ADC: StaticCell<Adc<'_, esp_hal::peripherals::ADC1<'_>, Async>> = StaticCell::new();

static ADC_READING: Signal<CriticalSectionRawMutex, f32> = Signal::new();
static AHT20_READING: Signal<CriticalSectionRawMutex, Option<aht20::Aht20Reading>> = Signal::new();
static BMP280_READING: Signal<CriticalSectionRawMutex, Option<bmp280::Bmp280Reading>> =
    Signal::new();

#[esp_rtos::main]
async fn main(spawner: Spawner) {
    let peripherals = esp_hal::init(esp_hal::Config::default());
    let mut rtc = Rtc::new(peripherals.LPWR);
    let boot_time = rtc.time_since_boot().as_millis();
    let wakeup_cause = esp_hal::rtc_cntl::wakeup_cause();

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

    // Setup RTC & Sleep Timer
    let timer = TimerWakeupSource::new(core::time::Duration::from_secs(10));
    let mut pin_1 = peripherals.GPIO1;

    {
        // XXX This doesnt seem to work reliably across devices - probably need to use external pull-up
        use esp_hal::gpio::RtcPinWithResistors;
        pin_1.rtcio_pullup(true);
    }

    let wakeup_pins: &mut [(&mut dyn RtcIoWakeupPinType, WakeupLevel)] =
        &mut [(&mut pin_1, WakeupLevel::Low)];

    #[cfg(not(feature = "esp32c6"))]
    let rtcio = RtcioWakeupSource::new(wakeup_pins);
    #[cfg(feature = "esp32c6")]
    let rtcio = Ext1WakeupSource::new(wakeup_pins);

    let boot_time = rtc.time_since_boot().as_millis();
    let wakeup_cause = esp_hal::rtc_cntl::wakeup_cause();

    #[cfg(not(feature = "esp32c6"))]
    let mut led = Output::new(peripherals.GPIO2, Level::Low, OutputConfig::default());
    #[cfg(feature = "esp32c6")]
    let mut led = Output::new(peripherals.GPIO15, Level::Low, OutputConfig::default());

    defmt::info!("INIT: wakeup={} boot={}", wakeup_cause, boot_time);

    match wakeup_cause {
        SleepSource::Undefined => {
            for i in 0..5 {
                defmt::info!("WAIT [{}]", i);
                Timer::after_millis(1000).await;
            }
        }
        _ => {}
    }

    // Turn LED on
    led.set_high();

    // ADC
    let adc_pin = peripherals.GPIO0;
    let mut adc_config = AdcConfig::new();
    let pin = adc_config.enable_pin_with_cal::<_, AdcCalBasic<_>>(adc_pin, Attenuation::_11dB);
    let adc = Adc::new(peripherals.ADC1, adc_config).into_async();
    let pin_static = PIN.init(pin);
    let adc_static = ADC.init(adc);
    spawner.spawn(adc_task(adc_static, pin_static)).unwrap();

    // Initialise I2C
    let i2c_config = i2c::master::Config::default().with_frequency(Rate::from_khz(100));
    let i2c = i2c::master::I2c::new(peripherals.I2C0, i2c_config)
        .expect("Error initailising I2C")
        .with_scl(peripherals.GPIO4)
        .with_sda(peripherals.GPIO5)
        .into_async();

    // Wait for bus to initialise
    Timer::after_millis(50).await;

    /*
    defmt::info!("Scan I2C bus: START");
    for addr in 0..=127 {
        if let Ok(_) = i2c.write_async(addr, &[0]).await {
            defmt::info!("Found I2C device at address: 0x{:02x}", addr);
        }
    }
    defmt::info!("Scan I2C bus: DONE");
    */

    // Create shared I2C bus
    let i2c_bus = I2C_BUS.init(Mutex::new(i2c));

    let aht20_device = I2cDevice::new(i2c_bus);
    spawner.spawn(aht20_task(aht20_device)).unwrap();

    let bmp280_device = I2cDevice::new(i2c_bus);
    spawner.spawn(bmp280_task(bmp280_device)).unwrap();

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

    let hub_address = find_hub(&mut esp_now).await;
    defmt::info!("ESP-NOW HUB: {}", format_mac(&hub_address));

    let mut msg = heapless::String::<256>::new();

    // Wait for update
    let (aht20, bmp280, adc) = join3(
        AHT20_READING.wait(),
        BMP280_READING.wait(),
        ADC_READING.wait(),
    )
    .await;

    let now = rtc.time_since_boot().as_millis();
    write!(
        &mut msg,
        "[{}] AHT20 <{:?}> BMP280<{:?}> ADC<{}>",
        now, aht20, bmp280, adc
    )
    .unwrap();
    defmt::info!("SENSOR UPDATE: {}", msg);

    // Send to Hub
    let status = esp_now.send_async(&hub_address, msg.as_bytes()).await;
    defmt::info!("ESP-NOW TX -> {}: {}", format_mac(&hub_address), status);

    // Wait for response
    loop {
        match with_timeout(Duration::from_millis(100), esp_now.receive_async()).await {
            Ok(r) => {
                if r.info.src_address == hub_address && r.info.dst_address != BROADCAST_ADDRESS {
                    defmt::info!(
                        "HUB RESPONSE: [{}]->[{}] >> {} [rssi={}]",
                        format_mac(&r.info.src_address),
                        format_mac(&r.info.dst_address),
                        core::str::from_utf8(r.data()).unwrap_or("UTF8 Error"),
                        r.info.rx_control.rssi
                    );
                }
            }
            Err(TimeoutError) => {
                break;
            }
        }
    }

    defmt::info!("SLEEPING:");
    controller.stop().unwrap();
    Timer::after_millis(500).await;
    rtc.sleep_deep(&[&timer, &rtcio]);
}

const V_REF: f32 = 1.1; // ADC refreence voltage
const K: f32 = 3.981; // Scaling factor for 11dB atten (really 12dB?)

#[embassy_executor::task]
async fn adc_task(
    adc: &'static mut Adc<'static, esp_hal::peripherals::ADC1<'static>, Async>,
    pin: &'static mut AdcPin<
        esp_hal::peripherals::GPIO0<'static>,
        esp_hal::peripherals::ADC1<'static>,
        AdcCalBasic<esp_hal::peripherals::ADC1<'static>>,
    >,
) {
    Timer::after_millis(100).await;
    // Take average of 5 readings
    let mut sum = 0.0;
    for _ in 0..5 {
        let adc_raw = adc.read_oneshot(pin).await;
        let adc_v = V_REF * K * (adc_raw as f32 / 4095.0);
        defmt::info!("ADC: {}V [{}]", adc_v, adc_raw);
        Timer::after_millis(10).await;
        sum += adc_v;
    }
    let adc_v = sum / 5.0;

    ADC_READING.signal(adc_v);
    defmt::info!("ADC Mean: {}V", adc_v);
}

#[embassy_executor::task]
async fn aht20_task(
    i2c: I2cDevice<'static, NoopRawMutex, esp_hal::i2c::master::I2c<'static, Async>>,
) {
    let mut aht20 = aht20::Aht20::new(i2c, 0x38);
    defmt::info!("AHT20 INIT");
    aht20.init().await.unwrap();

    Timer::after_millis(100).await;

    let r = aht20.read().await.ok();
    AHT20_READING.signal(r.clone());
    if let Some(aht20::Aht20Reading { temp, rh }) = r {
        defmt::info!("AHT20  TEMP: {}°C", temp);
        defmt::info!("       HUMIDITY: {}%", rh);
    }
}

#[embassy_executor::task]
async fn bmp280_task(
    i2c: I2cDevice<'static, NoopRawMutex, esp_hal::i2c::master::I2c<'static, Async>>,
) {
    let mut bmp280 = bmp280::Bmp280::new(i2c, 0x77);

    defmt::info!("BMP280 RESET: {}", bmp280.reset().await.unwrap());
    Timer::after_millis(100).await;
    defmt::info!("BMP280 INIT: {}", bmp280.init_default().await.unwrap());
    Timer::after_millis(100).await;
    let r = bmp280.measure().await.ok();

    BMP280_READING.signal(r.clone());
    if let Some(bmp280::Bmp280Reading { temp, pressure }) = r {
        defmt::info!("BMP280 TEMP: {}°C", temp);
        defmt::info!("       PRESSURE: {}hPa", pressure);
    }
    defmt::info!(
        "BMP280 LOW POWER MODE: {}",
        bmp280.init_low_power().await.unwrap()
    );
}

async fn find_hub(esp_now: &mut EspNow<'_>) -> [u8; 6] {
    let hub_persist = unsafe { HUB_ADDRESS };
    if hub_persist != [0; 6] {
        // Stored hub address
        defmt::info!("ESP-NOW ADD PEER: {}", format_mac(&hub_persist));
        esp_now
            .add_peer(PeerInfo {
                interface: esp_radio::esp_now::EspNowWifiInterface::Sta,
                peer_address: hub_persist,
                lmk: None,
                channel: None,
                encrypt: false,
            })
            .unwrap();
        hub_persist
    } else {
        loop {
            let r = esp_now.receive_async().await;
            defmt::info!(
                "ESP-NOW RX: [{}]->[{}] >> {} [rssi={}]",
                format_mac(&r.info.src_address),
                format_mac(&r.info.dst_address),
                core::str::from_utf8(r.data()).unwrap_or("UTF8 Error"),
                r.info.rx_control.rssi
            );
            if r.info.dst_address == BROADCAST_ADDRESS && r.data() == b"<<HUB>>" {
                defmt::info!(">> RX HUB BROADCAST");
                if !esp_now.peer_exists(&r.info.src_address) {
                    defmt::info!("ESP-NOW ADD PEER: {}", format_mac(&r.info.src_address));
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
                unsafe { HUB_ADDRESS = r.info.src_address };
                break r.info.src_address;
            }
        }
    }
}
